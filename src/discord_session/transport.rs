//! Background tasks for the existing Discord bridge protocol.

use crate::discord_bridge::{
    self, BridgeClientConfig, InviteStatus, PairingRegistrationResponse, PollRequestsResponse,
    RequestStatus,
};
use std::sync::mpsc::{self, Receiver};
use std::thread;

pub(super) trait BridgeTasks {
    fn register_pairing(
        &mut self,
        config: BridgeClientConfig,
        code: String,
    ) -> Receiver<Result<PairingRegistrationResponse, String>>;

    fn poll_requests(
        &mut self,
        config: BridgeClientConfig,
    ) -> Receiver<Result<PollRequestsResponse, String>>;

    fn report_request(
        &mut self,
        config: BridgeClientConfig,
        id: u64,
        status: RequestStatus,
        reason: Option<String>,
    );

    fn report_invite(&mut self, config: BridgeClientConfig, id: u64, status: InviteStatus);
}

pub(super) struct HttpBridgeTasks;

impl BridgeTasks for HttpBridgeTasks {
    fn register_pairing(
        &mut self,
        config: BridgeClientConfig,
        code: String,
    ) -> Receiver<Result<PairingRegistrationResponse, String>> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result =
                discord_bridge::register_pairing(&config, &code).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        receiver
    }

    fn poll_requests(
        &mut self,
        config: BridgeClientConfig,
    ) -> Receiver<Result<PollRequestsResponse, String>> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = discord_bridge::poll_requests(&config).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        receiver
    }

    fn report_request(
        &mut self,
        config: BridgeClientConfig,
        id: u64,
        status: RequestStatus,
        reason: Option<String>,
    ) {
        thread::spawn(move || {
            let _ = discord_bridge::report_request_status(&config, id, status, reason.as_deref());
        });
    }

    fn report_invite(&mut self, config: BridgeClientConfig, id: u64, status: InviteStatus) {
        thread::spawn(move || {
            let _ = discord_bridge::report_invite_status(&config, id, status);
        });
    }
}
