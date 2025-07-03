//! Communication session management
//!
//! This module defines the trait for tunneling messages between the game
//! engine and connected clients (players and hosts). The tunnel abstraction
//! allows for different communication mechanisms while maintaining a
//! consistent interface.

use super::{SyncMessage, UpdateMessage};

// pub enum Message {
//     Outgoing(OutgoingMessage),
//     State(StateMessage),
// }

/// Trait for sending messages through a communication tunnel
///
/// This trait abstracts the communication mechanism used to send messages
/// to connected clients. Implementations might use WebSockets, Server-Sent
/// Events, or other real-time communication protocols.
pub trait Tunnel {
    /// Sends an update message to the client
    ///
    /// Update messages notify clients about changes that affect their
    /// current view or state.
    ///
    /// # Arguments
    ///
    /// * `message` - The update message to send
    fn send_message(&self, message: &UpdateMessage);

    /// Sends a state synchronization message to the client
    ///
    /// Sync messages are used to synchronize the client's state with
    /// the current game state, typically when they connect or reconnect.
    ///
    /// # Arguments
    ///
    /// * `state` - The synchronization message to send
    fn send_state(&self, state: &SyncMessage);

    // fn send_multiple(&self, messages: &[Message]);

    /// Closes the communication tunnel
    ///
    /// This method should be called when the client disconnects or
    /// when the communication is no longer needed.
    fn close(self);
}
