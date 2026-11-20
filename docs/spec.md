# komotype – Spec (Entwurf v0.2)

Gamifizierter Tipp-Tracker für Windows mit nahtloser komorebi-bar-Integration
und optionaler ZMK-Firmware-Anbindung.

**Ziel:** Motivation, dauerhaft auf *eine* gewählte Tastatur (hier: Cornix) umzusteigen.
Alles ist so gebaut, dass andere Leute es mit ihrer eigenen Tastatur nutzen können.

---

## 1. Architektur

```
 ┌──────────────┐  raw-hid (USB/BLE)   ┌───────────────────────┐  Named Pipe   ┌──────────────────┐
 │ ZMK-Modul    │ ───────────────────▶ │ komotyped (Daemon)    │ ◀───────────▶ │ komorebi-bar Fork │
 │ (optional)   │ ◀─────────────────── │ Raw Input · SQLite    │  JSON-Lines   │ Widget "Komotype" │
 └──────────────┘                      │ Game-Engine · Toasts  │               └──────────────────┘
        ▲                              └───────────────────────┘
        │ Tasten                                ▲ Raw Input (WM_INPUT, pro Gerät)
   Tastatur ────────────────────────────────────┘
```

| Teil | Repo | Sprache | Aufgabe |
|---|---|---|---|
| `komotyped` | `komotype` | Rust | Eingaben erfassen, Stats, XP, Quests, Achievements, Toasts, IPC |
| `komotype-protocol` | `komotype` | Rust | Gemeinsame Typen: IPC-Nachrichten, raw-hid-Pakete |
| ZMK-Modul | `komotype` (`firmware/`, `zephyr/module.yml` im Root) | C | Layer, Akku beider Hälften, Tastenpositionen, HRM/Combos |
| Widget | Fork `komorebi` | Rust/egui | Anzeige; reiner IPC-Client, kleiner Diff |

**Warum Daemon statt im Bar-Prozess:** komorebi-bar läuft als ein Prozess pro Monitor
(doppelte Zählung), Stats überleben Bar-Neustarts, Fork bleibt klein und leicht rebasebar.
Widget startet den Daemon, falls er nicht läuft (Single-Instance-Mutex).

**Zwei Datenquellen, klar getrennt:**
- **Raw Input (Pflicht):** jede Tastatur, kein Firmware-Eingriff. Liefert WPM, Genauigkeit, Zeitstempel.
- **ZMK-Modul (optional):** Bonusdaten, die nur die Firmware kennt. Ohne Modul läuft alles außer den
  Firmware-Features.

---

## 2. Eingabe & Messung

- **Gerätewahl:** Raw Input liefert pro Tastendruck das Quellgerät (`hDevice` → Gerätepfad mit VID/PID).
  - Auswahl im Widget-Popup (Dropdown „Haupttastatur“) oder `komotyped devices` / Config.
  - Optional **Rivalen-Geräte** (z. B. die alte Tastatur): nur gezählt, keine XP (siehe 4.3).
- **Gezählt:** druckbare Zeichen, Space, Enter, Backspace. Modifier, Navigation, F-Tasten zählen nicht für WPM.
- **WPM live:** gleitendes 10-s-Fenster, Wörter = Zeichen / 5.
- **WPM Session:** Tippburst bis 5 s Pause; Bestwerte über Bursts ≥ 30 s (gegen Zufallsspitzen).
- **Genauigkeit:** `1 − Backspaces / druckbare Zeichen` (Näherung, gut genug für Trends).
- **Datenschutz:** nie Text, nie Tastenfolgen. Nur Zähler pro Taste/Minute/Tag. Keine App-Stats.

---

## 3. Firmware-Modul (ZMK, raw-hid)

Basis: [zzeneg/zmk-raw-hid](https://github.com/zzeneg/zmk-raw-hid) (Usage Page 0xFF60, 32-Byte-Reports).
Läuft auf der Central-Hälfte.

### 3.1 Daten
| Typ | Quelle in ZMK | Senden |
|---|---|---|
| Layer-Zustand | `zmk_layer_state_changed` | sofort bei Änderung |
| Akku Central + Peripheral(s) | `zmk_battery_state_changed`, `zmk_peripheral_battery_state_changed` | bei Änderung, max. 1×/min |
| Tastenpositionen | `zmk_position_state_changed` | gesammelt, alle ~2 s während Tippen |
| HRM Tap vs. Hold | Korrelation Position ↔ Keycode | gesammelt |
| Combos | Combo-Auslösung | gesammelt |

### 3.2 Protokoll (v1, Entwurf)
- Byte 0: Magic `0x4B` („K“), Byte 1: Protokollversion, Byte 2: Pakettyp, Rest Nutzdaten.
- Keyboard → Host: `HELLO` (Fähigkeiten, Anzahl Layer/Positionen), `LAYER`, `BATTERY`, `KEYSTATS`.
- Host → Keyboard: `SYNC` (vollen Zustand anfordern, z. B. nach Reconnect).
- Pakete werden gebündelt → wenig Funkverkehr, geringer Akkuverbrauch.

### 3.3 Risiken (→ Spike, Phase 0)
- Kommt der Vendor-HID-Kanal **über BLE unter Windows** an? Undokumentiert.
- HRM-Erkennung: ZMK hat kein öffentliches Hold-Tap-Event → Korrelation muss im Spike bewiesen werden.
- Combo-Erkennung: Reihenfolge der Listener prüfen.
- Kompatibilität mit dem Zephyr-Stand des Cornix-Boards.

---

## 4. Game-Design

Leitlinie: **Cornix benutzen muss sich jeden Tag lohnen, Fremdgehen soll spürbar etwas kosten,
aber nie frustrieren.**

### 4.1 XP & Level
- **XP-Quellen:** getippte Wörter (gewichtet nach Genauigkeit), Tagesquests, Achievements, Streak-Bonus.
- **Kurve:** frühe Level schnell (Lv 5 am ersten Tag), danach langsamer (`XP(L) ≈ 100·L^1.5`).
- **Ränge:** Titel alle 5 Level. Nächster Titel bleibt `???` bis erreicht.

### 4.2 Treue-Multiplikator (Kernmechanik gegen Rückfall)
- XP-Multiplikator wächst pro „treuem“ Tag (Anteil Haupttastatur ≥ 95 % aller Anschläge): +0,1×, max. 2,0×.
- Tag mit < 50 % Anteil: Multiplikator fällt auf 1,0×. Dazwischen: −0,3×.
- In der Bar sichtbar (Farbe/Flamme) → Verlustangst als Antrieb.
- Benötigt Zählung der anderen Tastaturen (nur Anzahl, nichts sonst). Abschaltbar.

### 4.3 Rivale („Geist der alten Tastatur“)
- WPM der Rivalen-Tastatur wird als 7-Tage-Schnitt gemessen.
- Anzeige im Popup: `Cornix 52 ▸ Rivale 78 – noch 26`. Ziel: den eigenen Geist überholen.

### 4.4 Streak
- Tag zählt ab N Anschlägen auf der Haupttastatur (Standard 2000, konfigurierbar).
- Alle 7 Tage ein Streak-Freeze (max. 2) → ein verpasster Tag ist kein Totalverlust.

### 4.5 Tagesquests
- 3 Quests pro Tag, um 4 Uhr neu gewürfelt, eine davon Firmware-bezogen (falls Modul aktiv).
- Sichtbar, sobald aktiv. Quest-Pool selbst nicht einsehbar.

### 4.6 Achievements (geheim)
- **Gesperrt:** nur Seltenheit sichtbar (Bronze/Silber/Gold/Legendär) als `???`. Kein Hinweis, kein Fortschritt.
- Zähler `23 / ?? entdeckt`, Gesamtzahl unbekannt. Es gibt zusätzlich versteckte, die nicht mal als `???` erscheinen.
- **Spoilerschutz:** Definitionen verschlüsselt im Repo (`achievements.enc`), Schlüssel im Binary.
  Werden vom Maintainer geschrieben, nie in Commits/Doku/Chat genannt.
- Engine: Regeln als Daten (Bedingungen über Stats/Events), damit neue Achievements ohne Code-Änderung dazukommen.
- **Umfang Start:** ~150. Seltenheit ca. 50 % Bronze / 30 % Silber / 15 % Gold / 5 % Legendär, plus geheime.
- **Pacing (Zielwerte bei regelmäßiger Nutzung):** Tag 1 eine Handvoll · 1 Monat ~25 % · 6 Monate ~50 % ·
  1 Jahr ~70 % · 3+ Jahre ~90 %. Einige legendäre als Fast-Unmöglich-Ziele.
- **Nachschub:** `achievements.enc` per Update erweiterbar; Zähler `?? ` bleibt dadurch ehrlich unbekannt.

### 4.7 Wochen-Challenge
- 1 pro Woche (Montag 4 Uhr), schwerer als Tagesquests, deutlich mehr XP.

### 4.8 Saisons
- Vierteljährlich. Eigene Quest-Pools und saisonale Achievements, nur in der Saison erreichbar.
- Verpasste saisonale Achievements kommen in späteren Saisons wieder (kein dauerhaftes FOMO).
- Saison-Rückblick im Popup am Saisonende.

### 4.9 Prestige
- Ab Maximallevel: Level freiwillig auf 1 zurücksetzen → dauerhafter Prestige-Stern in der Bar.
- Achievements, Stats, Rekorde bleiben. Prestige-Stufen haben eigene (geheime) Belohnungen.

---

## 5. Anzeige

### 5.1 Bar
```
⌨ 74 · Lv 12 ▰▰▰▱▱ 🔥1.6× · L2 · ◧87% ◨64%
```
- WPM live (Farbe nach Tempo), Level + XP-Balken, Treue-Multiplikator, aktiver Layer, Akku L/R.
- Elemente einzeln in der Bar-Config ein-/ausschaltbar.

### 5.2 Hover
WPM jetzt / heute / Bestwert, Genauigkeit, Streak, Quests-Fortschritt.

### 5.3 Klick → Popup (Tabs)
- **Heute:** Quests, XP heute, Treue-Anteil, Rivalenvergleich.
- **Stats:** Verläufe (WPM, Genauigkeit), Bestwerte, Heatmap physischer Tasten, Layer-Nutzung, HRM-Trefferquote.
- **Erfolge:** freigeschaltete mit Datum, gesperrte als `???` mit Seltenheit.
- **Einstellungen:** Haupttastatur, Rivalen, Streak-Schwelle.

### 5.4 Freischalten
Bar blinkt kurz in Seltenheitsfarbe + Windows-Toast (vom Daemon, funktioniert auch ohne Bar).

---

## 6. Konfiguration
- `%USERPROFILE%\.config\komotype\config.toml`: Geräte (VID/PID/Name), Schwellen, Feature-Flags.
- Daten: `%LOCALAPPDATA%\komotype\komotype.db` (SQLite).
- Bar-Widget in `komorebi.bar.json` wie jedes andere Widget: `{ "Komotype": { "enable": true, ... } }`.

---

## 7. Phasen
| # | Inhalt | Ergebnis |
|---|---|---|
| 0 | **Spike:** minimales ZMK-Modul sendet Layer + Akku über raw-hid; Test-CLI liest über USB **und BLE** | Machbarkeit geklärt |
| 1 | Daemon-Kern: Raw Input, Gerätewahl, WPM/Genauigkeit, SQLite, IPC | Zahlen im Terminal |
| 2 | Widget im Fork: Bar-Text, Hover, Popup-Stats | sichtbar in der Bar |
| 3 | Firmware-Modul komplett + Anbindung (Layer, Akku, Heatmap, HRM, Combos) | Firmware-Daten in der Bar |
| 4 | Game-Engine: XP, Level, Treue, Rivale, Streak, Tages-/Wochenquests, Saisons, Prestige | spielbar |
| 5 | Achievements (verdeckt) + Toasts | Belohnungen |
| 6 | Polishing für andere: README, Releases, Gerätewahl-UI | teilbar |
| – | Fork-Extra „Layout merken“ (Abschnitt 9), unabhängig, jederzeit einschiebbar | Layout überlebt Neustart |

## 8. Fork-Extra: Layout merken

Unabhängig vom Tracker, liegt im komorebi-Fork.

- **Problem:** Startlayout pro Workspace kommt fest aus `komorebi.json` (`workspaces[].layout`).
  Zur Laufzeit gewechselte Layouts gehen beim Neustart verloren.
- **Lösung:** Neue Option in `komorebi.json`, z. B. `"remember_layouts": true`.
  - Bei Layout-Wechsel (Event) wird pro Monitor + Workspace-Name das Layout in eine State-Datei
    (`%LOCALAPPDATA%\komorebi\layouts.json`) geschrieben.
  - Beim Start überschreibt die State-Datei das Layout aus `komorebi.json`.
  - `komorebi.json` selbst wird nie angefasst.
  - Reset per `komorebic forget-layouts` (neues Kommando) oder Datei löschen.
- **Vorab prüfen:** vorhandene Mechanismen (`quick-save`/`quick-load`, Workspace-Rules), ob sich darauf aufbauen lässt.
- So schreiben, dass ein Upstream-PR möglich ist.

## 9. Lizenzen & Credits
- komotype-eigener Code: MIT. Credits an komorebi (LGUG2Z), ZMK, zmk-raw-hid (zzeneg) im README.
- **komorebi-Fork:** Komorebi License 2.0 → Quellcode mit Änderungen teilen ok, nur nicht-kommerziell,
  **keine Binaries verteilen**. Fork-README bekommt Hinweis + Credits; Nutzer bauen selbst.
- **zmk-raw-hid:** keine Lizenz → kein Code kopieren, nur per `west.yml` als Abhängigkeit einbinden.
- Daemon (MIT) darf als Binary released werden; Widget im Fork nicht.

## 10. Offene Punkte
- Fallback für Akku ohne Modul: Windows-BLE-Batterie (nur Central) – lohnt sich?
