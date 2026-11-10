use mqtt5_protocol::packet::publish::PublishPacket;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Default)]
pub(super) struct OutboundInflight {
    publishes: HashMap<u16, PublishPacket>,
    send_order: VecDeque<u16>,
    awaiting_pubcomp: HashSet<u16>,
}

impl OutboundInflight {
    pub(super) fn len(&self) -> usize {
        self.publishes.len()
    }

    pub(super) fn contains(&self, packet_id: u16) -> bool {
        self.publishes.contains_key(&packet_id)
    }

    pub(super) fn get(&self, packet_id: u16) -> Option<&PublishPacket> {
        self.publishes.get(&packet_id)
    }

    pub(super) fn highest(&self) -> Option<u16> {
        self.publishes.keys().max().copied()
    }

    pub(super) fn insert(&mut self, packet_id: u16, publish: PublishPacket) {
        if self.publishes.insert(packet_id, publish).is_none() {
            self.send_order.push_back(packet_id);
        }
    }

    pub(super) fn insert_released(&mut self, packet_id: u16, publish: PublishPacket) {
        self.insert(packet_id, publish);
        self.awaiting_pubcomp.insert(packet_id);
    }

    pub(super) fn mark_released(&mut self, packet_id: u16) {
        if self.publishes.contains_key(&packet_id) {
            self.awaiting_pubcomp.insert(packet_id);
        }
    }

    pub(super) fn remove(&mut self, packet_id: u16) {
        if self.publishes.remove(&packet_id).is_some() {
            self.send_order.retain(|id| *id != packet_id);
        }
        self.awaiting_pubcomp.remove(&packet_id);
    }

    pub(super) fn take_unreleased(&mut self) -> Vec<(u16, PublishPacket)> {
        let unreleased = std::mem::take(&mut self.send_order)
            .into_iter()
            .filter(|packet_id| !self.awaiting_pubcomp.contains(packet_id))
            .filter_map(|packet_id| {
                self.publishes
                    .remove(&packet_id)
                    .map(|publish| (packet_id, publish))
            })
            .collect();
        self.publishes.clear();
        self.awaiting_pubcomp.clear();
        unreleased
    }
}
