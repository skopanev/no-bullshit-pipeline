# Diarization DER eval harness

Tune the FluidAudio diarizer config with **measured DER**, not by ear. The panel
found the live config (`clusteringThreshold=0.12`, `minSpeakers=2`, …) was set
blind; the no-regret reverts shipped, but `clusteringThreshold`, `minSpeakers`,
and the `numSpeakers` hint need data before changing.

## One-time setup

The sidecar takes config-override flags (added for this harness):

```
--diar-threshold <float>        # clusteringThreshold (live default 0.12)
--diar-exclude-overlap <bool>   # embeddingExcludeOverlap
--diar-num-speakers <int>       # force exact speaker count (0/omit = auto)
--diar-min-speakers <int>
--diar-min-seg-dur <float>
```

Rebuild the sidecar after pulling: `bun run build:sidecar:force`.

## Workflow

1. **Add clips.** Pick a handful of representative real recordings (1:1 calls,
   a 3-person call, a solo/monologue) and convert each to 16 kHz mono:

   ```sh
   ffmpeg -i ~/nbp-data/<id>/audio_mix.ogg -ar 16000 -ac 1 \
          scripts/diar-eval/clips/<name>.wav
   ```

2. **Seed ground truth.**

   ```sh
   cd scripts/diar-eval && python3 sweep.py --seed
   ```

   For each clip this writes `<name>.gt.tsv` (`start  end  speaker`) seeded from
   the current diarizer, plus `<name>.hint.txt` (the text per turn). **Open the
   `.gt.tsv` and correct the speaker column + obvious boundary errors by ear** —
   this is the human-labeled ground truth. Aim for ~5-10 clips, a few minutes
   each.

3. **Sweep.**

   ```sh
   python3 sweep.py
   ```

   Runs the sidecar over every `threshold × excludeOverlap × numSpeakers` combo
   on every labeled clip, prints mean DER sorted (lower = better), and names the
   winner. Edit the `GRID` in `sweep.py` to widen/narrow.

4. **Apply.** Set the winning values in `fluidaudio-sidecar/Sources/main.swift`
   (the `diarizerConfig.*` block) and rebuild.

## Files

- `der.py` — time-based DER (optimal speaker mapping). Also a CLI:
  `python3 der.py <gt.tsv> <sidecar_output.json>`.
- `sweep.py` — `--seed` and sweep driver.
- `clips/` — your `*.wav` + `*.gt.tsv` (wavs/hints are git-ignored; commit the
  `.gt.tsv` labels if you want them shared).

## Honest note

DER measures *who-spoke-when* against your labels. It can rank configs and kill
guesswork, but it can't lift the model's accuracy ceiling on heavily compressed
Opus/HFP mono call audio — that's a different (domain-adapted) model, not a knob.
