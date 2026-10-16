use vibex_core::{
    RemoteAuditListRequest, RemoteAuditRecord, RemoteCancelPairingOfferRequest,
    RemoteCreatePairingCodeRequest, RemoteCreatePairingCodeResponse,
    RemoteCreatePairingOfferRequest, RemoteCreatePairingOfferResponse, RemoteDeviceDetail,
    RemoteLanPairingDecision, RemoteLanPairingWindowSnapshot, RemotePairingOfferSummary,
    RemoteRenameDeviceRequest, RemoteRevokeDeviceRequest, RemoteStartLanPairingRequest,
};

use crate::{BackendBound, BackendFuture, MutationRequest};

pub trait DeviceBackend: BackendBound {
    fn create_pairing_offer(
        &self,
        request: MutationRequest<RemoteCreatePairingCodeRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingCodeResponse>;

    fn create_pairing_offer_v2(
        &self,
        request: MutationRequest<RemoteCreatePairingOfferRequest>,
    ) -> BackendFuture<'_, RemoteCreatePairingOfferResponse>;

    fn cancel_pairing_offer(
        &self,
        request: MutationRequest<RemoteCancelPairingOfferRequest>,
    ) -> BackendFuture<'_, RemotePairingOfferSummary>;

    /// Opens a LAN pairing window: the runtime advertises itself on the local
    /// network, and a device that finds it asks to pair rather than scanning an
    /// offer. The window is the discovery half of pairing, so nothing about the
    /// device is trusted until [`Self::decide_lan_pairing`] approves it.
    fn start_lan_pairing(
        &self,
        request: MutationRequest<RemoteStartLanPairingRequest>,
    ) -> BackendFuture<'_, RemoteLanPairingWindowSnapshot>;

    /// Opens a LAN pairing window that needs no published route.
    ///
    /// The window above stands on a Direct route the runtime already publishes
    /// and verifies, so a runtime with nothing published cannot open it. This
    /// one brings its own temporary listener and encrypted handshake, which is
    /// what lets a reader pair a phone over the local network when Remote Access
    /// has never been switched on. The window it returns is the same window:
    /// read it, settle it and cancel it through the calls above.
    fn start_zero_config_lan_pairing(
        &self,
        request: MutationRequest<vibex_core::RemoteStartLanPairingRequest>,
    ) -> BackendFuture<'_, RemoteLanPairingWindowSnapshot>;

    /// The window this runtime is advertising, or `None` when there is none.
    fn lan_pairing_window(&self) -> BackendFuture<'_, Option<RemoteLanPairingWindowSnapshot>>;

    /// Approves or refuses one device that asked to pair through a window.
    fn decide_lan_pairing(
        &self,
        request: MutationRequest<RemoteLanPairingDecision>,
    ) -> BackendFuture<'_, RemoteLanPairingWindowSnapshot>;

    /// Takes the advertisement down.
    fn cancel_lan_pairing(&self) -> BackendFuture<'_, ()>;

    fn list_devices(&self) -> BackendFuture<'_, Vec<RemoteDeviceDetail>>;

    /// The name this runtime publishes for itself.
    ///
    /// Every client renders it — the phone that discovers it on the network, the
    /// desktop that lists it, `/api/v2/info` — so a client that offers to change
    /// it has to be able to read it first. An empty answer is a runtime that
    /// predates the field, which the caller draws as no name rather than as an
    /// empty one.
    fn runtime_name(&self) -> BackendFuture<'_, String>;

    /// Whether an offer this client minted is still live, canceled, or claimed.
    ///
    /// The code and link are shown once and never stored, so this answers about
    /// the offer's state without handing the secret back: a client that drew the
    /// code is the one asking, and what it needs is to stop drawing it.
    fn pairing_offer_status(
        &self,
        request: vibex_core::RemotePairingOfferStatusRequest,
    ) -> BackendFuture<'_, vibex_core::RemotePairingOfferSummary>;

    /// The ways this runtime publishes itself for pairing, one entry per
    /// publishing method.
    ///
    /// An offer can only be minted over a published route, so when this is empty
    /// there is nothing to pair with — and that is the answer, not an error. The
    /// read stays available in exactly that state, which is why it is gated on
    /// its own capability rather than on `DevicePairing`.
    fn pairing_publications(
        &self,
    ) -> BackendFuture<'_, Vec<vibex_core::RemotePairingPublicationSummary>>;

    fn revoke_device(
        &self,
        request: MutationRequest<RemoteRevokeDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail>;

    /// Returns a revoked device to service with the grant it already holds.
    ///
    /// Revoking is reversible, and this is the verb that makes the promise
    /// real: the stored grant is kept, so a client that still holds the
    /// credential it was paired with reconnects without pairing again.
    fn restore_device(
        &self,
        request: MutationRequest<vibex_core::RemoteRestoreDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail>;

    /// Forgets a paired device, revoking its grant first when it still holds
    /// one and disconnecting it.
    ///
    /// Not the same as revoking: the record goes away, so the device is not
    /// merely inactive — and the audit history keeps the id it recorded while
    /// the trust store stops knowing the device.
    fn delete_device(
        &self,
        request: MutationRequest<vibex_core::RemoteDeleteDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail>;

    /// Renames a paired device in the runtime's trust store. The runtime owns
    /// the name, so the renamed device reads it back from its next handshake.
    fn rename_device(
        &self,
        request: MutationRequest<RemoteRenameDeviceRequest>,
    ) -> BackendFuture<'_, RemoteDeviceDetail>;

    /// Renames the runtime itself. Every client of that runtime renders the
    /// published name, so this is the rename that reaches all of them.
    fn rename_runtime(&self, request: MutationRequest<String>) -> BackendFuture<'_, String>;

    fn audit_records(
        &self,
        request: RemoteAuditListRequest,
    ) -> BackendFuture<'_, Vec<RemoteAuditRecord>>;
}
