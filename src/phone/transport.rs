//! How phones' bytes get to this computer. The shell and `arakun server` both
//! hold one, and neither minds which kind it is.

use super::relay::RelayClient;
use super::{ClientId, Server, Shared};

/// How phones' bytes get here: a server of our own on the network, or the
/// connection to the relay. Phones look the same to the shell either way.
pub enum Transport {
    Local(Server),
    Relay(RelayClient),
}

impl Transport {
    /// The port the server listens on; none through the relay.
    pub fn port(&self) -> Option<u16> {
        match self {
            Transport::Local(s) => Some(s.port()),
            Transport::Relay(_) => None,
        }
    }

    pub fn device_of(&self, id: ClientId) -> Option<String> {
        match self {
            Transport::Local(s) => s.device_of(id),
            Transport::Relay(r) => r.device_of(id),
        }
    }

    pub fn send(&self, id: ClientId, text: String) {
        match self {
            Transport::Local(s) => s.send(id, text),
            Transport::Relay(r) => r.send(id, text),
        }
    }

    pub fn broadcast(&self, text: &str) {
        match self {
            Transport::Local(s) => s.broadcast(text),
            Transport::Relay(r) => r.broadcast(text),
        }
    }

    pub fn client_count(&self) -> usize {
        match self {
            Transport::Local(s) => s.client_count(),
            Transport::Relay(r) => r.client_count(),
        }
    }

    pub fn connected_devices(&self) -> Vec<String> {
        match self {
            Transport::Local(s) => s.connected_devices(),
            Transport::Relay(r) => r.connected_devices(),
        }
    }

    pub fn disconnect_device(&self, device: &str) {
        match self {
            Transport::Local(s) => s.disconnect_device(device),
            Transport::Relay(r) => r.disconnect_device(device),
        }
    }

    /// The relay admits phones by ticket: it hears the list whenever the devices change.
    pub fn sync_tickets(&self, access: &Shared) {
        if let Transport::Relay(r) = self {
            r.set_tickets(access.lock().unwrap().ticket_hashes());
        }
    }

    /// The relay lets one phone in with the pairing code while it is open.
    pub fn sync_pairing(&self, access: &Shared) {
        if let Transport::Relay(r) = self {
            match access.lock().unwrap().pairing_hash() {
                Some((hash, left)) => r.open_pairing(hash, left),
                None => r.close_pairing(),
            }
        }
    }
}
