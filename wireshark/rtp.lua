-- RTP capture for UDP/RTP and ST 2110.
--
--   wireshark -X lua_script:wireshark/rtp.lua rtp.pcap
--
-- rtp.pcap is user DLT 148. One packet is:
--   direction u8 (1 send, 2 recv) | RTP packet

local rtp = Proto("etherquic_rtp", "EtherQuic RTP")

local f_dir = ProtoField.string("etherquic.rtp.dir", "Direction")
local f_ver = ProtoField.uint8("etherquic.rtp.version", "Version", base.DEC)
local f_pt = ProtoField.uint8("etherquic.rtp.pt", "Payload type", base.DEC)
local f_seq = ProtoField.uint16("etherquic.rtp.seq", "Sequence", base.DEC)
local f_ts = ProtoField.uint32("etherquic.rtp.timestamp", "Timestamp", base.DEC)
local f_ssrc = ProtoField.uint32("etherquic.rtp.ssrc", "SSRC", base.HEX)
local f_marker = ProtoField.uint8("etherquic.rtp.marker", "Marker", base.DEC)
local f_payload = ProtoField.bytes("etherquic.rtp.payload", "Payload")

rtp.fields = { f_dir, f_ver, f_pt, f_seq, f_ts, f_ssrc, f_marker, f_payload }

function rtp.dissector(buf, pinfo, tree)
    if buf:len() < 1 then
        return
    end
    pinfo.cols.protocol = "EtherQuic RTP"
    local root = tree:add(rtp, buf(), "EtherQuic RTP")
    local dir = buf(0, 1):uint()
    if dir == 1 then
        root:add(f_dir, buf(0, 1), "send")
    elseif dir == 2 then
        root:add(f_dir, buf(0, 1), "recv")
    else
        root:add(f_dir, buf(0, 1), "unknown")
    end
    if buf:len() < 13 then
        return
    end
    local b0 = buf(1, 1):uint()
    local b1 = buf(2, 1):uint()
    root:add(f_ver, buf(1, 1), bit.rshift(b0, 6))
    root:add(f_marker, buf(2, 1), bit.rshift(b1, 7))
    root:add(f_pt, buf(2, 1), bit.band(b1, 0x7f))
    root:add(f_seq, buf(3, 2))
    root:add(f_ts, buf(5, 4))
    root:add(f_ssrc, buf(9, 4))
    if buf:len() > 13 then
        root:add(f_payload, buf(13, buf:len() - 13))
    end
end

local user1 = 148
if wtap ~= nil and wtap.USER1 ~= nil then
    user1 = wtap.USER1
end
local encap = DissectorTable.get("wtap_encap")
if encap then
    encap:add(user1, rtp)
end
