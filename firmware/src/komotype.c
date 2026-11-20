/*
 * komotype ZMK module: reports keyboard state and key statistics to the komotype daemon over
 * raw HID. Protocol: docs/protocol.md
 *
 * SPDX-License-Identifier: MIT
 */

#include <string.h>

#include <zephyr/devicetree.h>
#include <zephyr/init.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/byteorder.h>

#include <raw_hid/events.h>

#include <dt-bindings/zmk/hid_usage_pages.h>
#include <zmk/behavior.h>
#include <zmk/event_manager.h>
#include <zmk/events/keycode_state_changed.h>
#include <zmk/events/layer_state_changed.h>
#include <zmk/events/position_state_changed.h>
#include <zmk/keymap.h>
#include <zmk/matrix.h>

#if IS_ENABLED(CONFIG_ZMK_BATTERY_REPORTING)
#include <zmk/battery.h>
#include <zmk/events/battery_state_changed.h>
#endif

#if IS_ENABLED(CONFIG_ZMK_SPLIT)
#include <zmk/split/central.h>
#endif

#include <zephyr/logging/log.h>
LOG_MODULE_DECLARE(zmk, CONFIG_ZMK_LOG_LEVEL);

#define KT_MAGIC 0x4B /* 'K' */
#define KT_VERSION 1
/* Version of this module, reported in STATE. 1 = layer + battery, 2 = + key statistics. */
#define KT_MODULE_VERSION 2

#define KT_MSG_STATE 0x01
#define KT_MSG_KEYSTATS 0x02
#define KT_MSG_HOLDTAP 0x03
#define KT_MSG_SYNC 0x80 /* host -> keyboard */

#define KT_HEADER 4
#define KT_MAX_PERIPHERALS 4
#define KT_BATTERY_UNKNOWN 0xFF
/* Key positions above this are not reported (fits a byte). */
#define KT_MAX_POSITIONS MIN(ZMK_KEYMAP_LEN, 255)
/* Hold-tap keys whose outcome is still open (pressed, or just released before the tap fires). */
#define KT_MAX_PENDING 8
#define KT_PENDING_TIMEOUT_MS 2000

#if IS_ENABLED(CONFIG_ZMK_SPLIT)
#define KT_PERIPHERAL_COUNT MIN(ZMK_SPLIT_CENTRAL_PERIPHERAL_COUNT, KT_MAX_PERIPHERALS)
#else
#define KT_PERIPHERAL_COUNT 0
#endif

/* Names of all hold-tap behaviors, to recognize them in keymap bindings. */
#define KT_HOLD_TAP_NAME(node) DEVICE_DT_NAME(node),
static const char *const hold_tap_names[] = {
    DT_FOREACH_STATUS_OKAY(zmk_behavior_hold_tap, KT_HOLD_TAP_NAME) NULL};

struct pending_hold_tap {
    bool used;
    uint8_t position;
    uint16_t hold_page, tap_page;
    uint32_t hold_id, tap_id;
    int64_t released_at; /* 0 while still pressed */
};

static uint8_t seq;
static struct k_work_delayable state_work;
static struct k_work_delayable stats_work;

/* Counts since the last KEYSTATS / HOLDTAP report. */
static uint8_t presses[KT_MAX_POSITIONS];
static uint8_t taps[KT_MAX_POSITIONS];
static uint8_t holds[KT_MAX_POSITIONS];
static struct pending_hold_tap pending[KT_MAX_PENDING];

#if IS_ENABLED(CONFIG_ZMK_SPLIT_BLE_CENTRAL_BATTERY_LEVEL_FETCHING)
/* ZMK reports 0 for peripherals it never heard from; only trust levels it actually received. */
static bool peripheral_battery_known[KT_MAX_PERIPHERALS];
#endif

static void header(uint8_t *report, uint8_t type) {
    report[0] = KT_MAGIC;
    report[1] = KT_VERSION;
    report[2] = type;
    report[3] = seq++;
}

static void send(uint8_t *report) {
    raise_raw_hid_sent_event(
        (struct raw_hid_sent_event){.data = report, .length = CONFIG_RAW_HID_REPORT_SIZE});
}

static uint8_t central_battery(void) {
#if IS_ENABLED(CONFIG_ZMK_BATTERY_REPORTING)
    return zmk_battery_state_of_charge();
#else
    return KT_BATTERY_UNKNOWN;
#endif
}

static uint8_t peripheral_battery(uint8_t source) {
#if IS_ENABLED(CONFIG_ZMK_SPLIT_BLE_CENTRAL_BATTERY_LEVEL_FETCHING)
    uint8_t level;
    if (source < KT_MAX_PERIPHERALS && peripheral_battery_known[source] &&
        zmk_split_central_get_peripheral_battery_level(source, &level) == 0) {
        return level;
    }
#endif
    return KT_BATTERY_UNKNOWN;
}

/*
 * STATE report layout (offsets in bytes, multi-byte values little endian):
 *  0 magic 'K' | 1 version | 2 type | 3 seq
 *  4 highest active layer | 5..8 active layer bitmask
 *  9 central battery % | 10 peripheral count | 11..14 peripheral battery %
 * 15..18 uptime in seconds | 19 module version (0 in module version 1)
 */
static void send_state(struct k_work *work) {
    uint8_t report[CONFIG_RAW_HID_REPORT_SIZE] = {0};

    header(report, KT_MSG_STATE);
    report[4] = zmk_keymap_highest_layer_active();
    sys_put_le32(zmk_keymap_layer_state(), &report[5]);
    report[9] = central_battery();
    report[10] = KT_PERIPHERAL_COUNT;
    for (uint8_t i = 0; i < KT_MAX_PERIPHERALS; i++) {
        report[11 + i] = i < KT_PERIPHERAL_COUNT ? peripheral_battery(i) : KT_BATTERY_UNKNOWN;
    }
    sys_put_le32((uint32_t)(k_uptime_get() / 1000), &report[15]);
    report[19] = KT_MODULE_VERSION;
    send(report);

    /* Heartbeat: resend the full state even if nothing changes. */
    k_work_reschedule(&state_work, K_SECONDS(CONFIG_ZMK_KOMOTYPE_HEARTBEAT_SECONDS));
}

static void expire_pending(void) {
    int64_t now = k_uptime_get();
    for (int i = 0; i < KT_MAX_PENDING; i++) {
        if (pending[i].used && pending[i].released_at &&
            now - pending[i].released_at > KT_PENDING_TIMEOUT_MS) {
            pending[i].used = false;
        }
    }
}

/*
 * KEYSTATS: byte 4 = entry count n, then n × (position, presses).
 * HOLDTAP:  byte 4 = entry count n, then n × (position, taps, holds).
 */
static void send_stats(struct k_work *work) {
    uint8_t report[CONFIG_RAW_HID_REPORT_SIZE];
    const int key_slots = (CONFIG_RAW_HID_REPORT_SIZE - KT_HEADER - 1) / 2;
    const int ht_slots = (CONFIG_RAW_HID_REPORT_SIZE - KT_HEADER - 1) / 3;
    int n;

    expire_pending();

    n = 0;
    memset(report, 0, sizeof(report));
    for (int pos = 0; pos < KT_MAX_POSITIONS; pos++) {
        if (!presses[pos]) {
            continue;
        }
        if (n == 0) {
            header(report, KT_MSG_KEYSTATS);
        }
        report[KT_HEADER + 1 + 2 * n] = pos;
        report[KT_HEADER + 2 + 2 * n] = presses[pos];
        presses[pos] = 0;
        if (++n == key_slots) {
            report[KT_HEADER] = n;
            send(report);
            memset(report, 0, sizeof(report));
            n = 0;
        }
    }
    if (n) {
        report[KT_HEADER] = n;
        send(report);
    }

    n = 0;
    memset(report, 0, sizeof(report));
    for (int pos = 0; pos < KT_MAX_POSITIONS; pos++) {
        if (!taps[pos] && !holds[pos]) {
            continue;
        }
        if (n == 0) {
            header(report, KT_MSG_HOLDTAP);
        }
        report[KT_HEADER + 1 + 3 * n] = pos;
        report[KT_HEADER + 2 + 3 * n] = taps[pos];
        report[KT_HEADER + 3 + 3 * n] = holds[pos];
        taps[pos] = 0;
        holds[pos] = 0;
        if (++n == ht_slots) {
            report[KT_HEADER] = n;
            send(report);
            memset(report, 0, sizeof(report));
            n = 0;
        }
    }
    if (n) {
        report[KT_HEADER] = n;
        send(report);
    }
}

static void request_state(void) {
    k_work_reschedule(&state_work, K_MSEC(CONFIG_ZMK_KOMOTYPE_DEBOUNCE_MS));
}

static void request_stats(void) {
    /* Not reschedule: keep the first deadline, so stats go out every interval while typing. */
    k_work_schedule(&stats_work, K_SECONDS(CONFIG_ZMK_KOMOTYPE_STATS_SECONDS));
}

static bool is_hold_tap(const char *behavior) {
    for (int i = 0; hold_tap_names[i] != NULL; i++) {
        if (strcmp(hold_tap_names[i], behavior) == 0) {
            return true;
        }
    }
    return false;
}

static uint16_t usage_page(uint32_t encoded) {
    uint16_t page = ZMK_HID_USAGE_PAGE(encoded);
    return page ? page : HID_USAGE_KEY;
}

static void on_position(const struct zmk_position_state_changed *ev) {
    if (ev->position >= KT_MAX_POSITIONS) {
        return;
    }
    uint8_t pos = ev->position;

    if (!ev->state) {
        for (int i = 0; i < KT_MAX_PENDING; i++) {
            if (pending[i].used && pending[i].position == pos && !pending[i].released_at) {
                pending[i].released_at = k_uptime_get();
            }
        }
        return;
    }

    if (presses[pos] < UINT8_MAX) {
        presses[pos]++;
    }
    request_stats();

    zmk_keymap_layer_id_t layer =
        zmk_keymap_layer_index_to_id(zmk_keymap_highest_layer_active());
    const struct zmk_behavior_binding *binding =
        zmk_keymap_get_layer_binding_at_idx(layer, pos);
    if (binding == NULL || binding->behavior_dev == NULL || !is_hold_tap(binding->behavior_dev)) {
        return;
    }
    expire_pending();
    for (int i = 0; i < KT_MAX_PENDING; i++) {
        if (!pending[i].used) {
            pending[i] = (struct pending_hold_tap){
                .used = true,
                .position = pos,
                .hold_page = usage_page(binding->param1),
                .hold_id = ZMK_HID_USAGE_ID(binding->param1),
                .tap_page = usage_page(binding->param2),
                .tap_id = ZMK_HID_USAGE_ID(binding->param2),
                .released_at = 0,
            };
            return;
        }
    }
}

/* Attributes a key code to the oldest open hold-tap key that can produce it. */
static void on_keycode(const struct zmk_keycode_state_changed *ev) {
    if (!ev->state) {
        return;
    }
    for (int i = 0; i < KT_MAX_PENDING; i++) {
        struct pending_hold_tap *p = &pending[i];
        if (!p->used) {
            continue;
        }
        if (p->tap_page == ev->usage_page && p->tap_id == ev->keycode) {
            if (taps[p->position] < UINT8_MAX) {
                taps[p->position]++;
            }
        } else if (p->hold_page == ev->usage_page && p->hold_id == ev->keycode) {
            if (holds[p->position] < UINT8_MAX) {
                holds[p->position]++;
            }
        } else {
            continue;
        }
        p->used = false;
        request_stats();
        return;
    }
}

static int komotype_listener(const zmk_event_t *eh) {
    const struct raw_hid_received_event *rx = as_raw_hid_received_event(eh);
    if (rx) {
        if (rx->length >= 3 && rx->data[0] == KT_MAGIC && rx->data[2] == KT_MSG_SYNC) {
            request_state();
        }
        return ZMK_EV_EVENT_BUBBLE;
    }

    const struct zmk_position_state_changed *pos = as_zmk_position_state_changed(eh);
    if (pos) {
        on_position(pos);
        return ZMK_EV_EVENT_BUBBLE;
    }

    const struct zmk_keycode_state_changed *key = as_zmk_keycode_state_changed(eh);
    if (key) {
        on_keycode(key);
        return ZMK_EV_EVENT_BUBBLE;
    }

#if IS_ENABLED(CONFIG_ZMK_SPLIT_BLE_CENTRAL_BATTERY_LEVEL_FETCHING)
    const struct zmk_peripheral_battery_state_changed *bat =
        as_zmk_peripheral_battery_state_changed(eh);
    if (bat && bat->source < KT_MAX_PERIPHERALS) {
        peripheral_battery_known[bat->source] = true;
    }
#endif

    /* Layer or battery changed. */
    request_state();
    return ZMK_EV_EVENT_BUBBLE;
}

ZMK_LISTENER(komotype, komotype_listener);
ZMK_SUBSCRIPTION(komotype, raw_hid_received_event);
ZMK_SUBSCRIPTION(komotype, zmk_layer_state_changed);
ZMK_SUBSCRIPTION(komotype, zmk_position_state_changed);
ZMK_SUBSCRIPTION(komotype, zmk_keycode_state_changed);
#if IS_ENABLED(CONFIG_ZMK_BATTERY_REPORTING)
ZMK_SUBSCRIPTION(komotype, zmk_battery_state_changed);
#endif
#if IS_ENABLED(CONFIG_ZMK_SPLIT_BLE_CENTRAL_BATTERY_LEVEL_FETCHING)
ZMK_SUBSCRIPTION(komotype, zmk_peripheral_battery_state_changed);
#endif

static int komotype_init(void) {
    k_work_init_delayable(&state_work, send_state);
    k_work_init_delayable(&stats_work, send_stats);
    k_work_schedule(&state_work, K_SECONDS(2));
    return 0;
}

SYS_INIT(komotype_init, APPLICATION, CONFIG_APPLICATION_INIT_PRIORITY);
