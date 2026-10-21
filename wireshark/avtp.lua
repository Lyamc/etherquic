-- AVTP capture for the AVB carrier.
--
--   wireshark -X lua_script:wireshark/avtp.lua avtp.pcap
--
-- avtp.pcap is Ethernet. EtherType 0x22F0 is an IEEE 1722 PDU.
-- Untagged frames start the PDU at byte 14. A VLAN tag starts it at byte 18.
-- Byte 0 of the PDU is the subtype: CVF 0x03, AAF 0x02, RVF 0x07.

local avtp = Proto("etherquic_avtp", "EtherQuic AVTP")

local f_subtype = ProtoField.uint8("etherquic.avtp.subtype", "Subtype", base.HEX)
local f_name = ProtoField.string("etherquic.avtp.name", "Format")
local f_seq = ProtoField.uint8("etherquic.avtp.sequence", "Sequence", base.DEC)
local f_pdu = ProtoField.bytes("etherquic.avtp.pdu", "PDU")

avtp.fields = { f_subtype, f_name, f_seq, f_pdu }

local function subtype_name(subtype)
    if subtype == 0x02 then
        return "AAF"
    elseif subtype == 0x03 then
        return "CVF"
    elseif subtype == 0x07 then
        return "RVF"
    end
    return "other"
end

function avtp.dissector(buf, pinfo, tree)
    if buf:len() < 14 then
        return
    end
    pinfo.cols.protocol = "EtherQuic AVTP"
    local root = tree:add(avtp, buf(), "EtherQuic AVTP")
    local offset = 14
    if buf(12, 2):uint() == 0x8100 and buf:len() >= 18 then
        offset = 18
    end
    if buf:len() <= offset then
        return
    end
    local subtype = buf(offset, 1):uint()
    root:add(f_subtype, buf(offset, 1))
    root:add(f_name, buf(offset, 1), subtype_name(subtype))
    if buf:len() > offset + 2 then
        root:add(f_seq, buf(offset + 2, 1))
    end
    root:add(f_pdu, buf(offset, buf:len() - offset))
end

DissectorTable.get("ethertype"):add(0x22F0, avtp)
