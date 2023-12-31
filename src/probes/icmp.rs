use std::net::Ipv4Addr;
use std::sync::Arc;
use tokio::sync::Mutex;

use async_trait::async_trait;
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
use crate::socket::AsyncSocket;

const ETHERNET_PACKET_MIN_SIZE: usize = MutableEthernetPacket::minimum_packet_size();
const IPV4_PACKET_MIN_SIZE: usize = Ipv4Packet::minimum_packet_size();
const ICMP_REQUEST_PACKET_SIZE: usize = ETHERNET_PACKET_MIN_SIZE
    + IPV4_PACKET_MIN_SIZE
    + MutableEchoRequestPacket::minimum_packet_size();
//TODO: check reply packet size in validation method
const ICMP_REPLY_PACKET_SIZE: usize = EchoReplyPacket::minimum_packet_size();

/// Implementation of `Probe` trait to enable a `Prober` to conduct ICMP echo probes.
#[derive(Debug)]
pub struct IcmpProbe {
    buf: Arc<Mutex<[u8; ICMP_REQUEST_PACKET_SIZE]>>,
}

impl IcmpProbe {
    pub fn many(count: usize, ethernet_conf: &EthernetConf) -> Result<Vec<Self>> {
        let mut v = Vec::new();
        for _ in 0..count {
            v.push(Self::new(ethernet_conf)?);
        }
        Ok(v)
    }

    pub fn new(ethernet_conf: &EthernetConf) -> Result<Self> {
        let mut buf = [0u8; ICMP_REQUEST_PACKET_SIZE];
        {
            let mut ethernet_packet = MutableEthernetPacket::new(&mut buf).expect("meow");
            log::trace!("ethernet_packet len: {}", ethernet_packet.packet().len());
            ethernet_packet.set_source(ethernet_conf.ethernet_info.source);
            ethernet_packet.set_destination(ethernet_conf.ethernet_info.destination);
            ethernet_packet.set_ethertype(ethernet_conf.ethernet_info.ethertype);

            log::trace!(
                "ethernet_packet payload len: {}",
                ethernet_packet.payload().len()
            );
            let mut ipv4_packet =
                MutableIpv4Packet::new(ethernet_packet.payload_mut()).expect("meow");
            log::trace!("ipv4_packetlen: {}", ipv4_packet.packet().len());
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

            log::trace!("ipv4 len: {}", MutableIpv4Packet::minimum_packet_size());
            log::trace!(
                "icmp min len: {}",
                MutableEchoRequestPacket::minimum_packet_size()
            );
            log::trace!("ipv4_packet total len: {}", ipv4_packet.get_total_length());
            log::trace!("ipv4_packet payload len: {}", ipv4_packet.payload().len());
            let mut icmp_packet = MutableEchoRequestPacket::new(ipv4_packet.payload_mut())
                .expect("the buf size should be exactly the minimum icmp packet size");
            icmp_packet.set_icmp_type(IcmpTypes::EchoRequest);
            icmp_packet.set_icmp_code(IcmpCode(0));
            icmp_packet.set_identifier(42);
        }

        Ok(Self {
            buf: Arc::new(Mutex::new(buf)),
        })
    }

    /// Updates the icmp buffer with the current icmp sequence and the new icmp checksum.
    async fn update_icmp_request_packet(&mut self, addr: &Ipv4Addr, seq: u16) {
        let mut buf = self.buf.lock().await;
        let slice = buf.as_mut_slice();
        let mut ethernet_packet = MutableEthernetPacket::new(slice).expect("meow");

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

#[derive(Debug, Serialize)]
pub struct IcmpOutput {
    addr: Ipv4Addr,
    seq: u16,
}

impl std::fmt::Display for IcmpOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{},{}", self.addr, self.seq)
    }
}

#[async_trait]
impl Probe for IcmpProbe {
    type Output = IcmpOutput;

    async fn send(&mut self, socket: AsyncSocket, tparams: &TargetParams) -> Result<()> {
        self.update_icmp_request_packet(&tparams.addr, tparams.seq)
            .await;
        match socket.send(self.buf.lock().await.as_slice()).await {
            Err(e) => {
                panic!("unhandled socket send error: {}", e);
            }
            Ok(length) => {
                log::trace!("sent {} bytes for request {}", length, tparams);
            }
        }
        Ok(())
    }

    /// Check that the given buffer is:
    /// * the right kind of IP packet (ICMP)
    /// * the right kind of ICMP packet (Echo Reply)
    /// If so, return the detected target params and probe output.
    fn validate_response(buf: &[u8]) -> Option<(TargetParams, <IcmpProbe as Probe>::Output)> {
        // check that it's an ICMP packet
        let ipv4_packet = Ipv4Packet::new(&buf)
            .expect("packet length already verified to be at least ICMP_REPLY_PACKET_SIZE");
        let source = &ipv4_packet.get_source();
        let protocol = ipv4_packet.get_next_level_protocol();
        match protocol {
            IpNextHeaderProtocols::Icmp => (),
            _ => {
                log::trace!("unexpected ip next level protocol number: {}", protocol);
                return None;
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
                    return None;
                }
            }
        }
        log::trace!("ipv4 header len: {}", ipv4_packet.get_header_length());
        log::trace!("ipv4 total len: {}", ipv4_packet.get_total_length());
        let ipv4_header_len =
            ipv4_packet.get_total_length() as usize - ipv4_packet.payload().len() as usize;

        log::trace!("ipv4 header len: {}", ipv4_header_len);
        let echo_reply_buf = &buf[ipv4_header_len..];
        log::trace!("echo reply buf len: {}", echo_reply_buf.len());
        let reply_packet = EchoReplyPacket::new(echo_reply_buf)
            .expect("packet length already verified to be at least ICMP_REPLY_PACKET_SIZE");

        let seq = reply_packet.get_sequence_number();

        Some((
            TargetParams {
                addr: source.clone(),
                seq,
            },
            IcmpOutput {
                addr: source.clone(),
                seq,
            },
        ))
    }
}
