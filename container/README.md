# etherquic-container

LOC objects from [draft-ietf-moq-loc](https://github.com/moq-wg/loc) (20 July 2026), framed so they can ride one QUIC stream without a MOQT session.

The payload of an object is the WebCodecs "internal data" of one encoded chunk: one Opus packet, or one video frame. Properties use the draft's type IDs and the MOQT even/odd key-value rule (even ID: one varint; odd ID: length-prefixed bytes).

```text
LOC Object {
  Group ID (i)
  Subgroup ID (i)
  Object ID (i)
  Public Properties Length (i)
  Public Properties
  Private Properties Length (i)
  Private Properties
  Payload Length (i)
  Payload
}
```

`(i)` is a QUIC variable-length integer. Objects are concatenated. A reader stops at the end of the stream.

| Property | ID | Value |
|---|---|---|
| Timescale | `0x08` | varint, units per second |
| Video Frame Marking | `0x09` | RFC 9626 mark, 1–4 bytes |
| Audio Level | `0x0C` | varint, low 8 bits are RFC 6464 |
| Video Config | `0x0D` | codec extradata |
| Audio Config | `0x0F` | Opus identification header (`OpusHead`…) |
| Timestamp | `0x10` | varint |

Public properties are the ones a relay is allowed to see. Private properties are the ones the draft allows to be encrypted with the payload. Both are encoded the same way here so a subscriber can still split them. Unknown IDs are preserved.

Video mapping (draft section 4.2): `group_id` increments on an independent frame, `object_id` increments inside the group. [`video_frame`](src/video.rs) builds that object. Opus uses the same objects from `etherquic-audio`: one packet per group, `object_id` 0, timescale 48000.
