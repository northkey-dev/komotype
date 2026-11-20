# Phase 0: Test raw HID over USB and Bluetooth (Cornix)

Ziel: Klären, ob die Statusberichte der Firmware unter Windows ankommen, per **USB und per Bluetooth**.

## Voraussetzung

ZMK läuft schon auf beiden Hälften (Cornix-Repo, `zmk/FLASHEN.md`, Stufen 0–5 erledigt).
Die Testfirmware ist die normale linke ZMK-Firmware plus komotype. Die rechte Hälfte bleibt unverändert.

Rückweg jederzeit: linke Hälfte wieder mit `2_zmk_links.uf2` flashen.

## 1. Flashen (nur linke Hälfte)

1. Kabel an die linke Hälfte, Doppelklick Reset → Laufwerk erscheint.
2. `4_zmk_links_komotype_v<N>.uf2` (höchste Version) aus `zmk/firmware/` draufziehen. Warten, bis das Laufwerk verschwindet.
3. Kurz tippen: verhält sich alles wie vorher?

## 2. Testtool vorbereiten (einmalig)

```
pip install hidapi
```

## 3. USB-Test

Linke Hälfte per Kabel am PC, rechte Hälfte an.

```
python C:\Soufian\Tastatur\komotype\tools\probe.py --list
python C:\Soufian\Tastatur\komotype\tools\probe.py
```

Erwartet:
- `--list` zeigt eine Zeile mit `[USB]`.
- Ohne `--list`: alle 5 s eine Zeile mit `layer 0`, Akku `central` und `peripherals`.
- Layer-Taste halten → sofort eine Zeile mit anderem Layer, beim Loslassen wieder `layer 0`.

## 4. Bluetooth-Test

1. Kabel ab. Cornix in Windows per Bluetooth koppeln (falls noch nicht geschehen).
2. Gleiche zwei Befehle wie oben.

Erwartet: dieselben Zeilen, nur mit `[Bluetooth]`.

Falls `--list` nichts findet: Cornix in Windows entfernen, neu koppeln, nochmal probieren.

## 5. Ergebnis melden

Bitte kopieren:
- Ausgabe von `--list` (USB und Bluetooth)
- ein paar Zeilen der Live-Ausgabe, auch mit Layerwechsel
- Fehler oder Zeilen mit `(gap: ...)`
- Fühlt sich die Tastatur anders an (Verzögerung, Aussetzer)?
