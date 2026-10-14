# etherquic-container

LOC objects from [draft-ietf-moq-loc](https://github.com/moq-wg/loc) (20 July 2026), concatenated on one QUIC stream. No MOQT session. A reader stops at the end of the stream.

The payload is one WebCodecs chunk: one Opus packet or one video frame. `(i)` is a QUIC variable-length integer. Properties use the draft's type IDs and the even/odd rule (even ID: one varint; odd ID: length-prefixed bytes).

```text
Group ID (i), Subgroup ID (i), Object ID (i)
Public Properties Length (i), Public Properties
Private Properties Length (i), Private Properties
Payload Length (i), Payload
```

| Property | ID | Value |
| --- | --- | --- |
| Timescale | `0x08` | varint, units per second |
| Video Frame Marking | `0x09` | RFC 9626 mark, 1–4 bytes |
| Audio Level | `0x0C` | varint, low 8 bits are RFC 6464 |
| Video Config | `0x0D` | codec extradata |
| Audio Config | `0x0F` | Opus identification header (`OpusHead`…) |
| Timestamp | `0x10` | varint |

Public properties are the ones a relay may see. Private properties are the ones the draft allows to encrypt with the payload. Both are encoded the same way here so a subscriber can still split them. Unknown IDs are kept.

Video (draft section 4.2): `group_id` increments on an independent frame, `object_id` increments inside the group. [`video_frame`](src/video.rs) builds that object. Opus, from `etherquic-audio`, is one packet per group, `object_id` 0, timescale 48000.
