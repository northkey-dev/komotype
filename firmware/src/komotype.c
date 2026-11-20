/*
 * komotype ZMK module: reports keyboard state to the komotype daemon over raw HID.
 * Protocol: docs/protocol.md
 *
 * SPDX-License-Identifier: MIT
 */

#include <string.h>

#include <zephyr/init.h>
#include <zephyr/kernel.h>
#include <zephyr/sys/byteorder.h>

#include <raw_hid/events.h>

#include <zmk/event_manager.h>
#include <zmk/keymap.h>
#include <zmk/events/layer_state_changed.h>

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

#define KT_MSG_STATE 0x01
#define KT_MSG_SYNC 0x80 /* host -> keyboard */

#define KT_MAX_PERIPHERALS 4
#define KT_BATTERY_UNKNOWN 0xFF

#if IS_ENABLED(CONFIG_ZMK_SPLIT)
#define KT_PERIPHERAL_COUNT MIN(ZMK_SPLIT_CENTRAL_PERIPHERAL_COUNT, KT_MAX_PERIPHERALS)
#else
#define KT_PERIPHERAL_COUNT 0
#endif

static uint8_t seq;
static struct k_work_delayable send_work;

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
    if (zmk_split_central_get_peripheral_battery_level(source, &level) == 0) {
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
 * 15..18 uptime in seconds
 */
static void send_state(struct k_work *work) {
    uint8_t report[CONFIG_RAW_HID_REPORT_SIZE] = {0};

    report[0] = KT_MAGIC;
    report[1] = KT_VERSION;
    report[2] = KT_MSG_STATE;
    report[3] = seq++;
    report[4] = zmk_keymap_highest_layer_active();
    sys_put_le32(zmk_keymap_layer_state(), &report[5]);
    report[9] = central_battery();
    report[10] = KT_PERIPHERAL_COUNT;
    for (uint8_t i = 0; i < KT_MAX_PERIPHERALS; i++) {
        report[11 + i] = i < KT_PERIPHERAL_COUNT ? peripheral_battery(i) : KT_BATTERY_UNKNOWN;
    }
    sys_put_le32((uint32_t)(k_uptime_get() / 1000), &report[15]);

    raise_raw_hid_sent_event(
        (struct raw_hid_sent_event){.data = report, .length = sizeof(report)});

    /* Heartbeat: resend the full state even if nothing changes. */
    k_work_reschedule(&send_work, K_SECONDS(CONFIG_ZMK_KOMOTYPE_HEARTBEAT_SECONDS));
}

static void request_send(void) {
    k_work_reschedule(&send_work, K_MSEC(CONFIG_ZMK_KOMOTYPE_DEBOUNCE_MS));
}

static int komotype_listener(const zmk_event_t *eh) {
    const struct raw_hid_received_event *rx = as_raw_hid_received_event(eh);
    if (rx) {
        if (rx->length >= 3 && rx->data[0] == KT_MAGIC && rx->data[2] == KT_MSG_SYNC) {
            request_send();
        }
        return ZMK_EV_EVENT_BUBBLE;
    }

    /* Layer or battery changed. */
    request_send();
    return ZMK_EV_EVENT_BUBBLE;
}

ZMK_LISTENER(komotype, komotype_listener);
ZMK_SUBSCRIPTION(komotype, raw_hid_received_event);
ZMK_SUBSCRIPTION(komotype, zmk_layer_state_changed);
#if IS_ENABLED(CONFIG_ZMK_BATTERY_REPORTING)
ZMK_SUBSCRIPTION(komotype, zmk_battery_state_changed);
#endif
#if IS_ENABLED(CONFIG_ZMK_SPLIT_BLE_CENTRAL_BATTERY_LEVEL_FETCHING)
ZMK_SUBSCRIPTION(komotype, zmk_peripheral_battery_state_changed);
#endif

static int komotype_init(void) {
    k_work_init_delayable(&send_work, send_state);
    k_work_schedule(&send_work, K_SECONDS(2));
    return 0;
}

SYS_INIT(komotype_init, APPLICATION, CONFIG_APPLICATION_INIT_PRIORITY);
