use std::net::Ipv4Addr;

use pnet::packet::{
    ethernet::MutableEthernetPacket,
    icmp::echo_reply::EchoReplyPacket,
    icmp::{echo_request::MutableEchoRequestPacket, IcmpCode, IcmpPacket, IcmpTypes},
    ip::IpNextHeaderProtocols,
    ipv4::{Ipv4Packet, MutableIpv4Packet},
    MutablePacket, Packet,
};
use serde::Serialize;

use crate::error::Result;
use crate::ethernet::EthernetConf;
use crate::prober::{Probe, TargetParams};

const ETHERNET_PACKET_MIN_SIZE: usize = MutableEthernetPacket::minimum_packet_size();
const IPV4_PACKET_MIN_SIZE: usize = Ipv4Packet::minimum_packet_size();
const ICMP_REQUEST_PACKET_SIZE: usize = ETHERNET_PACKET_MIN_SIZE
    + IPV4_PACKET_MIN_SIZE
    + MutableEchoRequestPacket::minimum_packet_size();
const ICMP_REPLY_PACKET_SIZE: usize = EchoReplyPacket::minimum_packet_size();

/// Abstraction for containing individual socket instances and pre-allocated Ethernet packet
/// buffer.
///
/// # Notes on Socket choice:
///
/// The reason I choose Domain::PACKET is that I have been reading the `zmap` paper recently[1] and
/// learned one of the tricks they use to achieve such high packet throughput is to use AF_PACKET
/// and manually construct Ethernet packets. This has two primary benefits:
///
/// First, it bypasses TCP/IP/whatever handling at the kernel level. This reduces kernel-specific
/// cpu and memory overhead in high throughput applications.
///
/// Second, in network mapping (or ICMP echo as is the case here) there are often very few
/// differences between the different request packets, which means one can optimize the
/// program to reduce memory allocations by pre-allocating request packet buffers rather
/// constructing them for every request. Request packet buffers are only needed for the duration of
/// each send call, after which they can re-used for subsequent requests.
///
/// To be honest, for a simple exercise like this the memory allocation optimization probably isn't
/// necessary, but I've been wondering how I would implement something zmap-like in Rust while
/// reading the original zmap paper and this interview is a good chance to do that.
///
/// [1] https://zmap.io/paper.pdf
#[derive(Debug)]
pub struct IcmpProbe {
    buf: [u8; ICMP_REQUEST_PACKET_SIZE],
}

impl IcmpProbe {
    pub fn many(count: usize) -> Result<Vec<Self>> {
        Ok(Vec::new())
    }

    pub fn new(
        ethernet_conf: &EthernetConf,
    ) -> Result<Self> {
        let mut buf = [0u8; ICMP_REQUEST_PACKET_SIZE];
        {
            let mut ethernet_packet = MutableEthernetPacket::new(&mut buf).expect("meow");
            log::debug!("ethernet_packet len: {}", ethernet_packet.packet().len());
            ethernet_packet.set_source(ethernet_conf.ethernet_info.source);
            ethernet_packet.set_destination(ethernet_conf.ethernet_info.destination);
            ethernet_packet.set_ethertype(ethernet_conf.ethernet_info.ethertype);

            log::debug!(
                "ethernet_packet payload len: {}",
                ethernet_packet.payload().len()
            );
            let mut ipv4_packet =
                MutableIpv4Packet::new(ethernet_packet.payload_mut()).expect("meow");
            log::debug!("ipv4_packetlen: {}", ipv4_packet.packet().len());
            ipv4_packet.set_version(4);
            ipv4_packet.set_source(ethernet_conf.interface.address);
            ipv4_packet.set_next_level_protocol(IpNextHeaderProtocols::Icmp);
            ipv4_packet.set_header_length(5);
            ipv4_packet.set_ttl(101); // not sure what a good value here would be so i picked this
            ipv4_packet.set_checksum(0); // not sure what a good value here would be so i picked this
            ipv4_packet.set_total_length(
                (MutableIpv4Packet::minimum_packet_size()
                    + MutableEchoRequestPacket::minimum_packet_size()) as u16,
            );
            // arbitrarily
            let checksum = pnet::packet::ipv4::checksum(
                &Ipv4Packet::new(ipv4_packet.packet()).expect("the buf size should be fine"),
            );
            ipv4_packet.set_checksum(checksum);

            log::debug!("ipv4 len: {}", MutableIpv4Packet::minimum_packet_size());
            log::debug!(
                "icmp min len: {}",
                MutableEchoRequestPacket::minimum_packet_size()
            );
            log::debug!("ipv4_packet total len: {}", ipv4_packet.get_total_length());
            log::debug!("ipv4_packet payload len: {}", ipv4_packet.payload().len());
            let mut icmp_packet = MutableEchoRequestPacket::new(ipv4_packet.payload_mut())
                .expect("the buf size should be exactly the minimum icmp packet size");
            icmp_packet.set_icmp_type(IcmpTypes::EchoRequest);
            icmp_packet.set_icmp_code(IcmpCode(0));
            icmp_packet.set_identifier(42);
        }

        Ok(Self {
            buf,
        })
    }

    /// Updates the icmp buffer with the current icmp sequence and the new icmp checksum.
    fn update_icmp_request_packet(&mut self, addr: &Ipv4Addr, seq: u16) {
        let mut ethernet_packet = MutableEthernetPacket::new(&mut self.buf).expect("meow");

        let mut ipv4_packet = MutableIpv4Packet::new(ethernet_packet.payload_mut()).expect("meow");
        ipv4_packet.set_destination(addr.clone());
        ipv4_packet.set_checksum(0);
        let checksum = pnet::packet::ipv4::checksum(
            &Ipv4Packet::new(ipv4_packet.packet()).expect("the buf size should be fine"),
        );
        ipv4_packet.set_checksum(checksum);

        let mut icmp_packet = MutableEchoRequestPacket::new(ipv4_packet.payload_mut())
            .expect("the buf size should be exactly the minimum icmp packet size");
        icmp_packet.set_sequence_number(seq);
        icmp_packet.set_checksum(0);

        let checksum = pnet::packet::icmp::checksum(
            &IcmpPacket::new(icmp_packet.packet())
                .expect("the buf size should be exactly the minimum icmp packet size"),
        );
        icmp_packet.set_checksum(checksum);
    }

}

#[derive(Serialize)]
pub struct IcmpOutput {}

impl Probe for IcmpProbe {
    type Output = IcmpOutput;

    fn update_buffer(&mut self, tparams: &TargetParams) -> Result<()> {
        self.update_icmp_request_packet(&tparams.addr, tparams.seq);
        Ok(())
    }

    fn get_buffer(&self) -> &[u8] {
        &self.buf
    }

    fn validate_response(&self, buf: &[u8], tparams: &TargetParams) -> Option<Self::Output> {
        if is_expected_packet(buf, &tparams.addr, tparams.seq) {
            Some(IcmpOutput {})
        } else {
            None
        }
    }
}

/// Check that the given buffer is:
/// * from the expected source IP
/// * the right kind of IP packet (ICMP)
/// * the right kind of ICMP packet (Echo Reply)
/// * the expected sequence number
fn is_expected_packet(reply_buf: &[u8], addr: &Ipv4Addr, seq: u16) -> bool {
    // check that it's an ICMP packet
    let ipv4_header_len = {
        let ipv4_packet = Ipv4Packet::new(&reply_buf)
            .expect("packet length already verified to be at least ICMP_REPLY_PACKET_SIZE");
        let source = &ipv4_packet.get_source();
        if source != addr {
            log::trace!("unexpected ipv4 source address: {source}");
            return false;
        }
        let protocol = ipv4_packet.get_next_level_protocol();
        match protocol {
            IpNextHeaderProtocols::Icmp => (),
            _ => {
                log::trace!("unexpected ip next level protocol number: {}", protocol);
                return false;
            }
        }
        // check that it's the right ICMP packet type
        {
            let icmp_packet = IcmpPacket::new(ipv4_packet.payload())
                .expect("packet length already verified to be at least ICMP_REPLY_PACKET_SIZE");
            match (icmp_packet.get_icmp_type(), icmp_packet.get_icmp_code()) {
                (IcmpTypes::EchoReply, IcmpCode(0)) => (),
                (t, c) => {
                    log::trace!("unexpected icmp (type, code): ({:?}, {:?})", t, c);
                    return false;
                }
            }
        }
        log::trace!("ipv4 header len: {}", ipv4_packet.get_header_length());
        log::trace!("ipv4 total len: {}", ipv4_packet.get_total_length());
        ipv4_packet.get_total_length() as usize - ipv4_packet.payload().len() as usize
    };

    log::trace!("ipv4 header len: {}", ipv4_header_len);
    let reply_buf = &reply_buf[ipv4_header_len..];
    log::trace!("echo reply buf len: {}", reply_buf.len());
    let reply_packet = EchoReplyPacket::new(reply_buf)
        .expect("packet length already verified to be at least ICMP_REPLY_PACKET_SIZE");

    reply_packet.get_sequence_number() == seq
}
