# Optional dirty-rectangle picture updates

A player can ask the host to spend encode work only on pixels that changed. Off by default. The host honours the request when it can, the same way it honours a codec pin: the offer carries the wish, the config record says what this session is actually doing.

Two codec families, one detection step, two transports.

| Family | Rows | When something moved | When nothing moved |
| --- | --- | --- | --- |
| Intra (still image) | MJPEG, MJ2K | Type-`R` patches of the dirty tiles, or a full `D` if the dirty area is large | Send nothing |
| Temporal (motion) | H.264, H.265, AV1 | Full `D` of the real RGB; the encoder’s P-skip is the dirty-rect | Send nothing |

PyroWave is intra (one independent picture per push) but its encoder is opened at full size, so a crop is not a legal next packet. First cut: treat it with the temporal row’s “full `D` when anything moved.” Type-`R` for PyroWave is later.

The pointer already rides `P` (cursor) records. It is not part of this plan.

## Already on the wire

Player offer (`ENEG`):

```text
knows pyrowave-cpu dirty-rects
dirty-rects 1
```

`knows dirty-rects` means the player can blit a type-`R` record. `dirty-rects 1` / `0` is the request. A missing `dirty-rects` line is an older player: the host falls back to its own Settings toggle.

`LiveOffer` already carries `dirty_rects`. A mid-session toggle resends the offer. `Offer::same_codecs` currently ignores that field so the encoder stays up; that is wrong once temporal GOP depends on the flag (see below).

Host encode loop already:

1. Compares this captured RGB to the last encoded RGB in 16×16 tiles (`media/src/dirty.rs`).
2. Unchanged → no picture record.
3. MJPEG / MJ2K, v2, player `knows dirty-rects`, dirty area under 50% and at most 12 merged rects → type-`R`.
4. Otherwise → full `D`.

Type-`R` payload after the record type byte:

```text
x u16le | y u16le | w u16le | h u16le | coded tile (a complete JPEG or JPEG 2000)
```

The player decodes the tile and blits it onto the last full frame. After a `C` config the canvas is empty, so the next picture must be a full `D`.

UI: Connect card quality slider plus “Send only changed tiles,” persisted as `dirty-rects=` in Settings. Same field as Display Capture.

## Detection (every codec)

Keep `dirty::plan`. Do not capture DXGI dirty rects in this pass; encode-side RGB compare is the source of truth.

| Plan | Meaning |
| --- | --- |
| `Unchanged` | Identical to last encoded picture. No `D` or `R`. |
| `Patches(rects)` | Small dirty area. Intra that can `patch_updates()` may send `R`. Everyone else sends `D`. |
| `Full` | First picture, size change, ≥50% dirty, or more than 12 rects. Always `D`. |

`previous` is cleared on encoder open and on size change, so the first picture after `C` is always `Full`.

## Intra transport (MJPEG, MJ2K)

Keep type-`R`. Rules:

1. After every `C`, the next picture is a full `D`.
2. `Unchanged`: send nothing.
3. `Patches`: one `R` per merged rect.
4. Never send `R` until a `D` has filled the canvas.
5. Do not send `R` on a temporal session.

`Video::patch_updates` stays MJPEG and MJ2K. PyroWave stays false until a crop encoder exists.

## Temporal transport (H.264, H.265, AV1)

The player keeps decoding every `D` as a full RGB frame. No composite step. No `R` on this stream.

When the offer wants dirty-rects, open the ffmpeg encoder with a long prediction chain and B-frames still off:

- `-bf 0` (already).
- GOP infinite or as large as the encoder allows (`-g 0` / a very large keyint). Hardware encoders often clamp this; a long finite GOP is acceptable.
- Intra-refresh where the encoder has it (`intra-refresh=1` on x264; NVENC refresh mode when present).
- Zerolatency / ull presets stay. Do not turn B-frames up.

`Unchanged` still means do not `push`. Anything dirty: `push` the real RGB. Skip-coded macroblocks are the encoder’s job. Size or codec change already drops the encoder, sends `C`, and the next picture is an IDR.

`same_codecs` must **not** ignore `want_dirty_rects` once GOP is tied to that flag. A live toggle reopens the encoder (same as a quality change) so the new GOP takes effect. Skip-unchanged can also flip on that reopen.

## Config echo

`StreamConfig` (`C` record) gains an optional line the player can show and that old players ignore:

```text
dirty-rects patches
dirty-rects pskip
dirty-rects off
```

| Value | Host behaviour |
| --- | --- |
| `patches` | Intra row, type-`R` when the plan says so |
| `pskip` | Temporal row, long GOP, skip-unchanged |
| `off` | Detection off; every captured picture is a full `D` |

The offer line is the request. This line is what the host is doing.

## Encoder arguments (temporal, dirty-rects on)

Per encoder, best effort:

| Encoder | GOP | Refresh |
| --- | --- | --- |
| libx264 | `keyint=infinite` in `-x264-params` (keep `repeat-headers=1:sliced-threads=1`) | `intra-refresh=1` |
| *_nvenc | large `-g`, keep `-bf 0 -delay 0` | intra-refresh if that build lists it |
| *_qsv / *_amf / *_videotoolbox | large `-g` if accepted | whatever the encoder documents |

Probe or accept failure to set infinite GOP. Do not fail the row because refresh is missing.

## Session rules

- Dirty-rects is a player request (`dirty-rects 1` in the offer / live offer). The host Settings toggle is only the fallback for an offer with no line, and the persisted default for this PC as player.
- Mixing `R` with a live P-decoder is forbidden.
- NUT stays the ffmpeg packetization for one `D` payload. No custom NUT flags, no SEI, no extra NAL types.
- No chroma-key, no alpha, no extra pixel row or column on the raster.
- Unchanged pictures send nothing. Do not add a hold/`S` record in this pass.
- Missed-frame detection and a player `refresh` / forced IDR request are out of this plan. Revisit later.

## Work

1. **Config echo.** Add `dirty-rects` to `StreamConfig` encode/decode/tests. Host fills it from the active row and the offer.
2. **Temporal GOP.** When `offer_wants_dirty_rects`, pass long GOP + intra-refresh into `encoder_args`. Keep `-bf 0`.
3. **Reopen on toggle.** Stop ignoring `want_dirty_rects` in `same_codecs` (or treat GOP-affecting changes as encoder restart). Quality already restarts; this should too.
4. **PyroWave.** No type-`R`. Skip-unchanged still applies; a dirty picture is a full `D`.
5. **Status.** Surface the config echo in the session line when present (`patches` / `pskip`).
6. **Tests.** Offer round-trip already covers `dirty-rects 0|1`. Add: config echo round-trip; `same_codecs` false when the request flips; host skip-unchanged with the request on and an identical frame; MJPEG type-`R` still blits; a temporal encoder is opened with a large GOP when the offer asks (inspect args or a probe helper).
7. **Architecture.md.** One paragraph: request in the offer, `patches` vs `pskip` in config, type-`R` for MJPEG/MJ2K, long GOP for H.264/H.265/AV1.

## Out

- DXGI / display-engine dirty rects as the detector.
- Alpha, fixed chroma-key, dynamic chroma-key, teletext pad strip.
- Infinite B-frames.
- Type-`R` or chroma-key for PyroWave (follow-up).
- Hold/`S` records for idle FPS stats.
- Player “missed a frame → force IDR” (follow-up).
- New ALPN or record types besides the config line.

## Tests (acceptance)

- Offer `dirty-rects 1` round-trips; missing line stays `None`; host fallback still uses Settings.
- MJPEG session with the request on: mouse-only motion yields `R` records; player canvas matches a full encode.
- Identical frames: no `D` or `R` for any codec.
- H.264 (sw) with the request on: encoder args include a large/infinite GOP and `-bf 0`; first picture after `C` is an IDR `D`; later identical pictures send nothing.
- Toggle off mid-session: encoder reopens, config says `off`, full `D` every captured picture.
- Old player (no `dirty-rects` line, no `knows dirty-rects`): no `R`; host Settings still gates skip-unchanged.
- Unknown config key `dirty-rects` is ignored by a decoder that does not parse it (existing unknown-key rule).
