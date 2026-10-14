-- EtherQuic Wireshark dissector.
--
--   wireshark -X lua_script:wireshark/etherquic.lua ether.pcap app.pcap
--
-- ether.pcap is Ethernet. EtherType 0x88B6 is followed by a 6-byte service
-- header and either a QUIC datagram or an EQDS discovery message. QUIC header
-- protection hides the packet number, so this stops before that field.
--
-- app.pcap is user DLT 147. One packet is one plaintext desktop record:
--   direction u8 (1 send, 2 recv) | 3 zero bytes | magic 4 | payload
-- Magics are EVID, EAUD, EINP, and EACK. An empty payload opens the stream.

local ether = Proto("etherquic", "EtherQuic")
local app = Proto("etherquic_app", "EtherQuic application")

local f_dst_svc = ProtoField.uint16("etherquic.dst_service", "Destination service", base.DEC)
local f_src_svc = ProtoField.uint16("etherquic.src_service", "Source service", base.DEC)
local f_plen = ProtoField.uint16("etherquic.payload_len", "Payload length", base.DEC)
local f_pad = ProtoField.bytes("etherquic.padding", "Ethernet padding")
local f_quic_hdr = ProtoField.uint8("etherquic.quic.first", "First byte", base.HEX)
local f_quic_form = ProtoField.string("etherquic.quic.form", "Header form")
local f_quic_fixed = ProtoField.uint8("etherquic.quic.fixed_bit", "Fixed bit", base.DEC)
local f_quic_type = ProtoField.string("etherquic.quic.packet_type", "Packet type")
local f_quic_ver = ProtoField.uint32("etherquic.quic.version", "Version", base.HEX)
local f_dcid_len = ProtoField.uint8("etherquic.quic.dcid_len", "DCID length", base.DEC)
local f_dcid = ProtoField.bytes("etherquic.quic.dcid", "Destination connection ID")
local f_scid_len = ProtoField.uint8("etherquic.quic.scid_len", "SCID length", base.DEC)
local f_scid = ProtoField.bytes("etherquic.quic.scid", "Source connection ID")
local f_token_len = ProtoField.uint64("etherquic.quic.token_len", "Token length", base.DEC)
local f_token = ProtoField.bytes("etherquic.quic.token", "Token")
local f_quic_len = ProtoField.uint64("etherquic.quic.length", "Length", base.DEC)
local f_protected = ProtoField.bytes("etherquic.quic.protected", "Protected remainder")
local f_versions = ProtoField.bytes("etherquic.quic.supported_versions", "Supported versions")

local f_eqds_magic = ProtoField.string("etherquic.eqds.magic", "Magic")
local f_eqds_ver = ProtoField.uint8("etherquic.eqds.version", "Version", base.DEC)
local f_eqds_kind = ProtoField.string("etherquic.eqds.kind", "Kind")
local f_eqds_id = ProtoField.uint32("etherquic.eqds.id", "Id", base.DEC)
local f_eqds_mac = ProtoField.ether("etherquic.eqds.mac", "Host MAC")
local f_eqds_l2 = ProtoField.uint16("etherquic.eqds.l2_service", "L2 service", base.DEC)
local f_eqds_l3 = ProtoField.uint16("etherquic.eqds.l3_port", "L3 port", base.DEC)
local f_eqds_name = ProtoField.string("etherquic.eqds.name", "Name")
local f_eqds_v4 = ProtoField.ipv4("etherquic.eqds.ipv4", "IPv4")
local f_eqds_v6 = ProtoField.ipv6("etherquic.eqds.ipv6", "IPv6")
local f_eqds_codec = ProtoField.string("etherquic.eqds.codec", "Codec")
local f_eqds_flags = ProtoField.uint8("etherquic.eqds.flags", "Flags", base.HEX)
local f_eqds_driver = ProtoField.string("etherquic.eqds.driver", "L2 driver")

local f_dir = ProtoField.string("etherquic.app.dir", "Direction")
local f_magic = ProtoField.string("etherquic.app.magic", "Stream")
local f_jpeg = ProtoField.bytes("etherquic.app.jpeg", "JPEG")
local f_rate = ProtoField.uint32("etherquic.app.sample_rate", "Sample rate", base.DEC)
local f_channels = ProtoField.uint16("etherquic.app.channels", "Channels", base.DEC)
local f_samples = ProtoField.uint32("etherquic.app.samples", "PCM samples", base.DEC)
local f_pcm = ProtoField.bytes("etherquic.app.pcm", "PCM")
local f_eqin_ver = ProtoField.uint8("etherquic.app.eqin_version", "EQIN version", base.DEC)
local f_ev_len = ProtoField.uint16("etherquic.app.event_len", "Event length", base.DEC)
local f_ev_time = ProtoField.uint64("etherquic.app.time_us", "Time", base.DEC)
local f_ev_kind = ProtoField.string("etherquic.app.event", "Event")
local f_key_down = ProtoField.uint8("etherquic.app.key_down", "Down", base.DEC)
local f_key_source = ProtoField.string("etherquic.app.key_source", "Key source")
local f_key_code = ProtoField.uint32("etherquic.app.key_code", "Key code", base.HEX)
local f_x = ProtoField.uint16("etherquic.app.x", "X", base.DEC)
local f_y = ProtoField.uint16("etherquic.app.y", "Y", base.DEC)
local f_dx = ProtoField.int16("etherquic.app.dx", "DX", base.DEC)
local f_dy = ProtoField.int16("etherquic.app.dy", "DY", base.DEC)
local f_buttons = ProtoField.uint8("etherquic.app.buttons", "Buttons", base.HEX)
local f_wheel_y = ProtoField.int16("etherquic.app.wheel_y", "Wheel Y", base.DEC)
local f_wheel_x = ProtoField.int16("etherquic.app.wheel_x", "Wheel X", base.DEC)
local f_pad_index = ProtoField.uint8("etherquic.app.gamepad", "Gamepad", base.DEC)
local f_pad_buttons = ProtoField.uint32("etherquic.app.pad_buttons", "Gamepad buttons", base.HEX)
local f_axis = ProtoField.int16("etherquic.app.axis", "Axis", base.DEC)
local f_sync = ProtoField.uint8("etherquic.app.sync", "Lock keys", base.HEX)
local f_ack_time = ProtoField.uint64("etherquic.app.ack_time", "Time", base.DEC)
local f_ack_kind = ProtoField.string("etherquic.app.ack_kind", "Ack kind")
local f_ack_code = ProtoField.uint32("etherquic.app.ack_code", "Code", base.HEX)
local f_ack_result = ProtoField.string("etherquic.app.ack_result", "Result")
local f_ack_detail = ProtoField.uint8("etherquic.app.ack_detail", "Detail", base.DEC)
local f_body = ProtoField.bytes("etherquic.app.body", "Body")

ether.fields = {
    f_dst_svc, f_src_svc, f_plen, f_pad,
    f_quic_hdr, f_quic_form, f_quic_fixed, f_quic_type, f_quic_ver,
    f_dcid_len, f_dcid, f_scid_len, f_scid, f_token_len, f_token, f_quic_len, f_protected, f_versions,
    f_eqds_magic, f_eqds_ver, f_eqds_kind, f_eqds_id, f_eqds_mac, f_eqds_l2, f_eqds_l3, f_eqds_name,
    f_eqds_v4, f_eqds_v6, f_eqds_codec, f_eqds_flags, f_eqds_driver,
}
app.fields = {
    f_dir, f_magic, f_jpeg, f_rate, f_channels, f_samples, f_pcm, f_eqin_ver,
    f_ev_len, f_ev_time, f_ev_kind, f_key_down, f_key_source, f_key_code,
    f_x, f_y, f_dx, f_dy, f_buttons, f_wheel_y, f_wheel_x,
    f_pad_index, f_pad_buttons, f_axis, f_sync,
    f_ack_time, f_ack_kind, f_ack_code, f_ack_result, f_ack_detail, f_body,
}

local ef_short = ProtoExpert.new("etherquic.short", "Truncated EtherQuic frame", expert.group.MALFORMED, expert.severity.ERROR)
local ef_app = ProtoExpert.new("etherquic.app.short", "Truncated application record", expert.group.MALFORMED, expert.severity.ERROR)
ether.experts = { ef_short }
app.experts = { ef_app }

local VIDEO = { [1] = "Pyrowave", [2] = "MJPEG", [3] = "H.264", [4] = "H.265", [5] = "AV1" }
local AUDIO = { [1] = "PCM", [2] = "Opus" }
local DRIVER = { [1] = "libpcap", [2] = "npcap", [3] = "membrane" }
local LONG_TYPE = { [0] = "Initial", [1] = "0-RTT", [2] = "Handshake", [3] = "Retry" }
local KEY_SOURCE = { [0] = "HID", [1] = "Set 1", [2] = "Keysym" }
local EVENT_KIND = { [1] = "Key", [2] = "Pointer", [3] = "Delta", [4] = "Gamepad", [5] = "Sync" }
local ACK_KIND = { [1] = "Key", [2] = "Pointer", [3] = "Other" }
local ACK_RESULT = { [0] = "Applied", [1] = "Ignored", [2] = "Failed" }
local AXIS = { "LX", "LY", "RX", "RY", "LT", "RT" }

local function hasbit(value, mask)
    return math.floor(value / mask) % 2 == 1
end

local function service_name(port)
    if port == 4433 then return "play" end
    if port == 4434 then return "audio" end
    if port == 4435 then return "input" end
    if port == 4440 then return "discovery" end
    return nil
end

local function service_text(port)
    local name = service_name(port)
    if name then return string.format("%d %s", port, name) end
    return tostring(port)
end

local function flag_text(flags)
    local parts = {}
    if hasbit(flags, 0x01) then parts[#parts + 1] = "encode" end
    if hasbit(flags, 0x02) then parts[#parts + 1] = "decode" end
    if hasbit(flags, 0x04) then parts[#parts + 1] = "cpu" end
    if hasbit(flags, 0x08) then parts[#parts + 1] = "gpu" end
    if hasbit(flags, 0x10) then parts[#parts + 1] = "ffmpeg" end
    if hasbit(flags, 0x20) then parts[#parts + 1] = "gstreamer" end
    if hasbit(flags, 0x40) then parts[#parts + 1] = "libopus" end
    if #parts == 0 then return "none" end
    return table.concat(parts, " ")
end

-- QUIC variable-length integer. Returns value, next offset.
local function quic_varint(buf, off)
    if off >= buf:len() then return nil end
    local first = buf(off, 1):uint()
    local prefix = math.floor(first / 64)
    local len = 2 ^ prefix
    if off + len > buf:len() then return nil end
    local value = first % 64
    for i = 1, len - 1 do
        value = value * 256 + buf(off + i, 1):uint()
    end
    return value, off + len
end

local function add_cids(buf, tree, off)
    if off >= buf:len() then return nil end
    local dcid_len = buf(off, 1):uint()
    tree:add(f_dcid_len, buf(off, 1))
    off = off + 1
    if dcid_len > 20 or off + dcid_len > buf:len() then return nil end
    if dcid_len > 0 then
        tree:add(f_dcid, buf(off, dcid_len))
        off = off + dcid_len
    end
    if off >= buf:len() then return nil end
    local scid_len = buf(off, 1):uint()
    tree:add(f_scid_len, buf(off, 1))
    off = off + 1
    if scid_len > 20 or off + scid_len > buf:len() then return nil end
    if scid_len > 0 then
        tree:add(f_scid, buf(off, scid_len))
        off = off + scid_len
    end
    return off
end

local function dissect_quic(buf, pinfo, tree)
    if buf:len() < 1 then return end
    local first = buf(0, 1):uint()
    local quic = tree:add(ether, buf(), "QUIC")
    quic:add(f_quic_hdr, buf(0, 1))
    if first < 128 then
        quic:add(f_quic_form, "short")
        -- Bit 6 is the fixed bit and is not header-protected. The low 5 bits
        -- (spin, reserved, key phase, packet number length) are protected,
        -- and a short header does not carry the DCID length.
        local fixed = math.floor(first / 64) % 2
        quic:add(f_quic_fixed, fixed)
        if buf:len() > 1 then
            quic:add(f_protected, buf(1, buf:len() - 1))
        end
        pinfo.cols.info:append(" QUIC short header (protected)")
        return
    end
    quic:add(f_quic_form, "long")
    local fixed = math.floor(first / 64) % 2
    quic:add(f_quic_fixed, fixed)
    if buf:len() < 5 then
        quic:add_proto_expert_info(ef_short)
        return
    end
    local version = buf(1, 4):uint()
    quic:add(f_quic_ver, buf(1, 4))
    local off = add_cids(buf, quic, 5)
    if not off then
        quic:add_proto_expert_info(ef_short)
        return
    end
    if version == 0 then
        quic:add(f_quic_type, "Version negotiation")
        if off < buf:len() then
            quic:add(f_versions, buf(off, buf:len() - off))
        end
        pinfo.cols.info:append(" QUIC version negotiation")
        return
    end
    -- Bits 5-4 are the long packet type. Bits 3-0 are protected.
    local kind = math.floor((first % 64) / 16)
    local type_name = LONG_TYPE[kind] or string.format("type %d", kind)
    quic:add(f_quic_type, type_name)
    if kind == 3 then
        if buf:len() >= off + 16 then
            local token_len = buf:len() - off - 16
            if token_len > 0 then
                quic:add(f_token, buf(off, token_len))
            end
        end
        pinfo.cols.info:append(" QUIC Retry")
        return
    end
    if kind == 0 then
        local token_len, next_off = quic_varint(buf, off)
        if not token_len then
            quic:add_proto_expert_info(ef_short)
            return
        end
        quic:add(f_token_len, token_len)
        off = next_off
        if off + token_len > buf:len() then
            quic:add_proto_expert_info(ef_short)
            return
        end
        if token_len > 0 then
            quic:add(f_token, buf(off, token_len))
            off = off + token_len
        end
    end
    if kind == 0 or kind == 1 or kind == 2 then
        local length, next_off = quic_varint(buf, off)
        if not length then
            quic:add_proto_expert_info(ef_short)
            return
        end
        quic:add(f_quic_len, length)
        off = next_off
    end
    if off < buf:len() then
        quic:add(f_protected, buf(off, buf:len() - off))
    end
    pinfo.cols.info:append(" QUIC " .. type_name .. " (protected)")
end

local function dissect_codecs(buf, tree, off, count, names, label)
    for _ = 1, count do
        if off + 2 > buf:len() then return nil end
        local id = buf(off, 1):uint()
        local flags = buf(off + 1, 1):uint()
        local name = names[id] or string.format("id %d", id)
        local item = tree:add(f_eqds_codec, buf(off, 2), string.format("%s %s (%s)", label, name, flag_text(flags)))
        item:add(f_eqds_flags, buf(off + 1, 1))
        off = off + 2
    end
    return off
end

local function dissect_eqds(buf, pinfo, tree)
    local eqds = tree:add(ether, buf(), "EQDS")
    eqds:add(f_eqds_magic, buf(0, 4))
    if buf:len() < 10 then
        eqds:add_proto_expert_info(ef_short)
        return
    end
    local version = buf(4, 1):uint()
    local kind = buf(5, 1):uint()
    eqds:add(f_eqds_ver, buf(4, 1))
    local kind_name = "unknown"
    if kind == 1 then kind_name = "announce" end
    if kind == 2 then kind_name = "offer" end
    eqds:add(f_eqds_kind, kind_name)
    eqds:add(f_eqds_id, buf(6, 4))
    pinfo.cols.info:append(" EQDS " .. kind_name)
    if kind ~= 2 or buf:len() == 10 then
        return
    end
    local off = 10
    if off + 6 + 2 + 2 + 1 > buf:len() then
        eqds:add_proto_expert_info(ef_short)
        return
    end
    eqds:add(f_eqds_mac, buf(off, 6))
    off = off + 6
    eqds:add(f_eqds_l2, buf(off, 2))
    off = off + 2
    eqds:add(f_eqds_l3, buf(off, 2))
    off = off + 2
    local name_len = buf(off, 1):uint()
    off = off + 1
    if off + name_len > buf:len() then
        eqds:add_proto_expert_info(ef_short)
        return
    end
    if name_len > 0 then
        eqds:add(f_eqds_name, buf(off, name_len))
        off = off + name_len
    end
    if off >= buf:len() then return end
    local n4 = buf(off, 1):uint()
    off = off + 1
    for _ = 1, n4 do
        if off + 4 > buf:len() then
            eqds:add_proto_expert_info(ef_short)
            return
        end
        eqds:add(f_eqds_v4, buf(off, 4))
        off = off + 4
    end
    if off >= buf:len() then return end
    local n6 = buf(off, 1):uint()
    off = off + 1
    for _ = 1, n6 do
        if off + 16 > buf:len() then
            eqds:add_proto_expert_info(ef_short)
            return
        end
        eqds:add(f_eqds_v6, buf(off, 16))
        off = off + 16
    end
    if version < 2 or off >= buf:len() then return end
    local nvideo = buf(off, 1):uint()
    off = off + 1
    off = dissect_codecs(buf, eqds, off, nvideo, VIDEO, "video")
    if not off then
        eqds:add_proto_expert_info(ef_short)
        return
    end
    if off >= buf:len() then return end
    local naudio = buf(off, 1):uint()
    off = off + 1
    off = dissect_codecs(buf, eqds, off, naudio, AUDIO, "audio")
    if not off then
        eqds:add_proto_expert_info(ef_short)
        return
    end
    if off >= buf:len() then return end
    local driver = buf(off, 1):uint()
    local driver_name = DRIVER[driver] or string.format("%d", driver)
    eqds:add(f_eqds_driver, buf(off, 1), driver_name)
end

function ether.dissector(buf, pinfo, tree)
    pinfo.cols.protocol = "EtherQuic"
    if buf:len() < 6 then
        tree:add_proto_expert_info(ef_short)
        return
    end
    local dst_svc = buf(0, 2):uint()
    local src_svc = buf(2, 2):uint()
    local payload_len = buf(4, 2):uint()
    local root = tree:add(ether, buf(), "EtherQuic")
    root:add(f_dst_svc, buf(0, 2)):append_text(" (" .. service_text(dst_svc) .. ")")
    root:add(f_src_svc, buf(2, 2)):append_text(" (" .. service_text(src_svc) .. ")")
    root:add(f_plen, buf(4, 2))
    pinfo.cols.info = string.format(
        "%s -> %s",
        service_text(src_svc),
        service_text(dst_svc)
    )
    if 6 + payload_len > buf:len() then
        root:add_proto_expert_info(ef_short)
        return
    end
    local payload = buf(6, payload_len)
    if payload_len >= 4 and payload(0, 4):string() == "EQDS" then
        dissect_eqds(payload:tvb(), pinfo, root)
    else
        dissect_quic(payload:tvb(), pinfo, root)
    end
    local pad = buf:len() - 6 - payload_len
    if pad > 0 then
        root:add(f_pad, buf(6 + payload_len, pad))
    end
end

local function call_jpeg(tvb, pinfo, tree)
    if tvb:len() < 2 then return end
    if tvb(0, 1):uint() ~= 0xFF or tvb(1, 1):uint() ~= 0xD8 then return end
    for _, name in ipairs({ "jpeg", "image-jfif" }) do
        local ok, dissector = pcall(Dissector.get, name)
        if ok and dissector then
            dissector:call(tvb, pinfo, tree)
            return
        end
    end
    tree:add(f_jpeg, tvb())
end

local function add_le_u16(tree, field, buf, off)
    tree:add_le(field, buf(off, 2))
    return off + 2
end

local function add_le_i16(tree, field, buf, off)
    tree:add_le(field, buf(off, 2))
    return off + 2
end

local function dissect_pointer(tree, buf, off, absolute)
    if off + 9 > buf:len() then return off end
    if absolute then
        off = add_le_u16(tree, f_x, buf, off)
        off = add_le_u16(tree, f_y, buf, off)
    else
        off = add_le_i16(tree, f_dx, buf, off)
        off = add_le_i16(tree, f_dy, buf, off)
    end
    tree:add(f_buttons, buf(off, 1))
    off = off + 1
    off = add_le_i16(tree, f_wheel_y, buf, off)
    off = add_le_i16(tree, f_wheel_x, buf, off)
    return off
end

local function dissect_eqin(buf, tree)
    if buf:len() < 5 or buf(0, 4):string() ~= "EQIN" then
        tree:add(f_body, buf())
        return
    end
    tree:add(f_eqin_ver, buf(4, 1))
    local off = 5
    while off + 2 <= buf:len() do
        local len = buf(off, 2):le_uint()
        local item = tree:add_le(f_ev_len, buf(off, 2))
        off = off + 2
        if off + len > buf:len() or len < 9 then
            item:add_proto_expert_info(ef_app)
            return
        end
        local ev = buf(off, len)
        item:add_le(f_ev_time, ev(0, 8))
        local kind = ev(8, 1):uint()
        local kind_name = EVENT_KIND[kind] or string.format("kind %d", kind)
        item:add(f_ev_kind, kind_name)
        item:set_text("Event: " .. kind_name)
        local body = 9
        if kind == 1 and ev:len() >= 15 then
            item:add(f_key_down, ev(body, 1))
            local source = ev(body + 1, 1):uint()
            item:add(f_key_source, KEY_SOURCE[source] or tostring(source))
            item:add_le(f_key_code, ev(body + 2, 4))
        elseif kind == 2 then
            dissect_pointer(item, ev, body, true)
        elseif kind == 3 then
            dissect_pointer(item, ev, body, false)
        elseif kind == 4 and ev:len() >= body + 17 then
            item:add(f_pad_index, ev(body, 1))
            item:add_le(f_pad_buttons, ev(body + 1, 4))
            local axis_off = body + 5
            for i = 1, 6 do
                local value = ev(axis_off, 2):le_int()
                local axis = item:add_le(f_axis, ev(axis_off, 2))
                axis:set_text(string.format("%s: %d", AXIS[i], value))
                axis_off = axis_off + 2
            end
        elseif kind == 5 and ev:len() >= body + 1 then
            item:add(f_sync, ev(body, 1))
        elseif ev:len() > body then
            item:add(f_body, ev(body, ev:len() - body))
        end
        off = off + len
    end
end

local function dissect_eack(buf, tree)
    if buf:len() ~= 15 then
        tree:add(f_body, buf())
        tree:add_proto_expert_info(ef_app)
        return
    end
    tree:add_le(f_ack_time, buf(0, 8))
    local kind = buf(8, 1):uint()
    tree:add(f_ack_kind, ACK_KIND[kind] or tostring(kind))
    tree:add_le(f_ack_code, buf(9, 4))
    local result = buf(13, 1):uint()
    tree:add(f_ack_result, ACK_RESULT[result] or tostring(result))
    tree:add(f_ack_detail, buf(14, 1))
end

local function dissect_eaud(buf, tree)
    if buf:len() < 6 then
        tree:add_proto_expert_info(ef_app)
        return
    end
    local rate = buf(0, 4):le_uint()
    local channels = buf(4, 2):le_uint()
    tree:add_le(f_rate, buf(0, 4))
    tree:add_le(f_channels, buf(4, 2))
    local pcm_bytes = buf:len() - 6
    local samples = math.floor(pcm_bytes / 2)
    tree:add(f_samples, samples):set_text(string.format("PCM samples: %d", samples))
    if pcm_bytes > 0 then
        tree:add(f_pcm, buf(6, pcm_bytes))
    end
    if channels > 0 then
        local frames = math.floor(samples / channels)
        tree:append_text(string.format(" %d Hz %d ch %d frames", rate, channels, frames))
    end
end

function app.dissector(buf, pinfo, tree)
    pinfo.cols.protocol = "EtherQuic"
    if buf:len() < 8 then
        tree:add_proto_expert_info(ef_app)
        return
    end
    local dir = buf(0, 1):uint()
    local dir_name = "other"
    if dir == 1 then dir_name = "send" end
    if dir == 2 then dir_name = "recv" end
    local magic = buf(4, 4):string()
    local root = tree:add(app, buf(), "EtherQuic " .. magic .. " " .. dir_name)
    root:add(f_dir, dir_name)
    root:add(f_magic, buf(4, 4))
    local payload = buf(8, buf:len() - 8):tvb()
    if payload:len() == 0 then
        pinfo.cols.info = magic .. " " .. dir_name .. " stream open"
        return
    end
    pinfo.cols.info = magic .. " " .. dir_name .. " " .. payload:len() .. " bytes"
    if magic == "EVID" then
        call_jpeg(payload, pinfo, root)
    elseif magic == "EAUD" then
        dissect_eaud(payload, root)
    elseif magic == "EINP" then
        dissect_eqin(payload, root)
    elseif magic == "EACK" then
        dissect_eack(payload, root)
    else
        root:add(f_body, payload())
    end
end

DissectorTable.get("ethertype"):add(0x88B6, ether)
local user0 = 147
if wtap ~= nil and wtap.USER0 ~= nil then
    user0 = wtap.USER0
end
local encap = DissectorTable.get("wtap_encap")
if encap then
    encap:add(user0, app)
end
