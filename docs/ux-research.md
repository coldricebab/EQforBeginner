# UX research: public Dirac Live workflow

Reviewed 2026-07-18. This research uses only public user workflow material; it does
not infer or reproduce proprietary algorithms, UI assets, or internal behavior.

Public Dirac guidance consistently presents a progressive path: grant microphone
access, discover/select a device, select the microphone and calibration, begin at a
low level, capture the sweet spot and guided surrounding positions, react immediately
to clipping/low-SNR errors, inspect/edit a target, calculate, inspect a predicted or
corrected response, export to a named slot, and save the project. Current Dirac Live
copy calls the old “Volume Calibration” step “Measurement Levels” and “Select
Arrangement” “Select Sweet Spot”. Those labels are research context, not UI assets to
copy. The useful interaction patterns for this product are:

1. Keep one primary task and one forward action visible per step.
2. Make device/path readiness a gate before measurement.
3. Show microphone positions spatially and retain progress across positions.
4. Let a simple target control lead; put detailed controls behind an advanced view.
5. Separate calculation from deployment and show the predicted response before export.
6. Save enough project state to revisit target choices without re-measuring.
7. Reduce level automatically when returning to calibration and require an explicit
   action before raising a safety-limited master level.
8. Autosave accepted positions and keep calculation separate from device-slot export.

EQforBeginner deliberately adds stricter safety semantics: rejected measurements are
never silently averaged, predicted results never count as verified, and final export
requires a real 48 kHz filtered remeasurement in a later phase.

Dirac's public placement guidance recommends measuring the central sweet spot first,
using a real three-dimensional listening volume rather than tightly clustered points,
and matching microphone orientation to its calibration file. EQforBeginner applies those
principles to its independent P0+5 layout; it does not reproduce Dirac's 9/13/17-point
diagrams. Dirac's manual also describes moving a detected impulse peak to display-time
0 ms. EQforBeginner must not adopt that convention for stored data: any future display
alignment must remain separate from the preserved original timeline and arrival time.

Sources:

- [Dirac Live public user manual](https://helpdesk.dirac.com/en/dirac-live/Dirac-Live-User-Manual-1eb2)
- [Dirac Live Quick Start](https://helpdesk.dirac.com/en/dirac-live/Dirac-Live-Quick-Start-Guide-fb62)
- [Dirac measurement order](https://helpdesk.dirac.com/en/dirac-live/In-what-order-should-I-measure-the-positions-cbe)
- [Dirac Live 3.13.4 terminology change](https://helpdesk.dirac.com/en/dirac-live/Dirac-Live-3134-LATEST-Software-Changelog-bfed)
- [Dirac Live Processor: where to start](https://helpdesk.dirac.com/en/dirac-art/Room-Correction-Suite-Where-do-I-start-2f39)
- [Dirac ART setup guide](https://helpdesk.dirac.com/en/dirac-art/Setup-Guide-c3cb)
- [Dirac Live Bass Control filter design](https://helpdesk.dirac.com/en/dirac-bass-control/Filter-Design-c592)
- [Dirac output-level safety lock](https://helpdesk.dirac.com/en/dirac-room-correction/Why-is-there-a-red-lock-on-the-Master-volume-in-the-Volume-Calibration-page-c178)
- [Dirac calibration-page automatic attenuation](https://helpdesk.dirac.com/en/dirac-room-correction/Why-does-the-volume-decrease-significantly-when-returning-to-the-Volume-Calibration-step-8319)

These sources are references for ease of use only. Product copy must say
“limited-band multi-position room correction” and “guided single-sub
integration”; it must not claim feature equivalence.
The current public quick-start presets use more positions than this project's P0+5;
the P0+5 layout is our independent protocol, not a copied preset.
