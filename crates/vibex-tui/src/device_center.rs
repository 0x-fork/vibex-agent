//! The devices page: pairing and the devices already paired, on one surface.
//!
//! Pairing and the trust store used to be two destinations reached by two
//! palette verbs — a dialog that showed one code and a page that listed what
//! came of it. They are one job: the reader opens this page to let a phone or a
//! second computer in, and the list of what is already in is how they check
//! that it worked. So the page keeps the desktop's two halves and separates
//! them with a tab strip rather than a keypress, the way the configuration
//! centre separates its modules.
//!
//! What the pairing half offers is not the reader's choice, it is the runtime's:
//! a phone can always scan an offer, a window can be advertised on the local
//! network when the runtime can reach Direct, and a second desktop can only be
//! handed a code or a link by a headless runtime — a desktop shell pairs
//! phones, it does not accept another desktop as a client. The row that cannot
//! work is absent rather than disabled: a page that lists a verb the runtime
//! will refuse teaches the reader to distrust the page.
//!
//! Everything here is pure: the projections answer rows, the state machine
//! answers where the cursor is and what is open, and neither performs I/O. The
//! reducer turns a row into an effect and the renderer draws the rows the
//! projection answered, which is what keeps the mouse and the keyboard acting
//! on the same object.

use vibex_core::{
    RemoteDevicePermissionLevel, RemoteLanPairingRequestState, RemoteLanPairingWindowSnapshot,
    RemoteServerKind,
};

use crate::app::Availability;
use crate::locale::Strings;

/// Which half of the page is open.
///
/// The pairing half comes first because it is what the page is for; a reader
/// who came back to check a device is one `→` away, and the module they were on
/// is remembered while they are there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, PartialOrd, Ord)]
pub enum DeviceTab {
    #[default]
    Pair,
    Devices,
}

impl DeviceTab {
    pub const ALL: [Self; 2] = [Self::Pair, Self::Devices];

    pub const fn key(self) -> &'static str {
        match self {
            Self::Pair => "pair",
            Self::Devices => "devices",
        }
    }

    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::Pair => strings.devices_tab_pair(),
            Self::Devices => strings.devices_title(),
        }
    }
}

/// One way to pair, as the pairing tab lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PairMethod {
    /// Show an offer as a QR code: the phone reads it with its camera.
    QrCode,
    /// Advertise the runtime on the local network: the phone finds it and asks,
    /// and the request is approved here.
    LocalNetwork,
    /// Show the offer as a one-time code and a link: a second desktop pastes
    /// the link into its add-runtime form.
    CodeOrLink,
}

impl PairMethod {
    pub const fn key(self) -> &'static str {
        match self {
            Self::QrCode => "qr",
            Self::LocalNetwork => "lan",
            Self::CodeOrLink => "code",
        }
    }

    /// The row's title: the reader's action, not the mechanism.
    pub const fn label(self, strings: Strings) -> &'static str {
        match self {
            Self::QrCode => strings.devices_pair_qr(),
            Self::LocalNetwork => strings.devices_pair_lan(),
            Self::CodeOrLink => strings.devices_pair_code(),
        }
    }

    /// What the row is for: who can use it, and what they need.
    pub const fn scope(self, strings: Strings) -> &'static str {
        match self {
            Self::QrCode => strings.devices_pair_qr_scope(),
            Self::LocalNetwork => strings.devices_pair_lan_scope(),
            Self::CodeOrLink => strings.devices_pair_code_scope(),
        }
    }

    /// The effect that opens this row's panel.
    pub const fn opening(self) -> Opening {
        match self {
            Self::QrCode | Self::CodeOrLink => Opening::Offer,
            Self::LocalNetwork => Opening::Window,
        }
    }
}

/// What opening a pairing row starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opening {
    /// Mint a pairing offer.
    Offer,
    /// Open a LAN pairing window.
    Window,
}

/// What one press on a pairing row did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// The row's panel was opened, and this is what it needs.
    Opened(Opening),
    /// The row's panel was closed. A closed window panel asks for the
    /// advertisement to come down: a runtime left discoverable behind a page
    /// that says it is not looking would pair a phone the reader never saw.
    Closed { cancel_window: bool },
}

/// The pairing rows this runtime can actually offer, in the order they are
/// drawn.
///
/// A row is present only when it can work here. The runtime's kind decides the
/// desktop row — only a headless runtime accepts a desktop client — and the
/// capabilities decide the rest, because whether a runtime can advertise itself
/// on the local network is a fact about the runtime rather than about the
/// reader's network.
pub fn pair_methods(
    runtime_kind: RemoteServerKind,
    pairing: Availability,
    lan_pairing: Availability,
) -> Vec<PairMethod> {
    if pairing != Availability::Available {
        return Vec::new();
    }
    let mut methods = vec![PairMethod::QrCode];
    if lan_pairing == Availability::Available && runtime_kind == RemoteServerKind::Desktop {
        methods.push(PairMethod::LocalNetwork);
    }
    if runtime_kind == RemoteServerKind::Headless {
        methods.push(PairMethod::CodeOrLink);
    }
    methods
}

/// One offer, as the panel draws it.
///
/// The code and the link are the same offer seen twice: the code is what the
/// reader reads out, the link is what the QR encodes and what a desktop pastes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingOffer {
    pub code: String,
    /// The link a phone opens, and the one a desktop pastes into its
    /// add-runtime form.
    ///
    /// The runtime answers with a bare `#/pair/…` fragment and leaves the
    /// transport to the client, which is the side that knows which of the
    /// offer's routes it is showing. The `vibex://open/…` form around it is not
    /// decoration: the mobile scanner only accepts a `vibex://` link, so a
    /// fragment on its own is a code no camera would act on.
    pub url: String,
    pub permission: RemoteDevicePermissionLevel,
    /// When the runtime stops accepting the code. An offer that is drawn past
    /// its deadline is a code the reader will type into a phone that refuses
    /// it, so the page stops drawing one.
    pub expires_at_ms: i64,
}

impl PairingOffer {
    /// The offer the runtime answered with, as the panel shows it.
    pub fn from_response(response: &vibex_core::RemoteCreatePairingOfferResponse) -> Self {
        let summary = &response.offer.summary;
        let transport = summary
            .direct_candidates
            .iter()
            .map(|candidate| candidate.transport)
            .chain(
                summary
                    .relay_candidate
                    .iter()
                    .map(|candidate| candidate.transport),
            )
            .find(|transport| *transport != vibex_core::RemotePairingTransport::Unknown)
            .unwrap_or(vibex_core::RemotePairingTransport::Direct);
        let fragment = response
            .launch_fragment
            .strip_prefix('#')
            .unwrap_or(&response.launch_fragment);
        Self {
            code: response.offer.one_time_challenge.clone(),
            url: format!(
                "vibex://open/{}#{fragment}",
                pairing_transport_name(transport)
            ),
            permission: summary.permission_level,
            expires_at_ms: summary.expires_at_ms,
        }
    }
}

/// The path segment that tells the mobile app which route the offer carries.
const fn pairing_transport_name(transport: vibex_core::RemotePairingTransport) -> &'static str {
    match transport {
        vibex_core::RemotePairingTransport::Direct => "direct",
        vibex_core::RemotePairingTransport::Tailnet => "tailnet",
        vibex_core::RemotePairingTransport::SelfHostedRelay => "self_hosted_relay",
        vibex_core::RemotePairingTransport::Unknown => "direct",
    }
}

/// A pairing action that failed, filed under the row it was taken from.
///
/// The code travels with the message because the recovery copy is chosen from
/// it: "no Direct route" and "you lack permission" are the same sentence only
/// to a reader who has not been told which one happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingError {
    pub method: PairMethod,
    pub code: String,
    pub message: String,
}

/// The live facts behind the pairing tab.
#[derive(Debug, Clone, Default)]
pub struct Pairing {
    /// The method whose request is in flight. Only one at a time: two windows,
    /// or an offer and a window, would be two advertisements of the same
    /// runtime and the reader would not know which one the phone answered.
    pub busy: Option<PairMethod>,
    /// The offer the QR and the link are drawn from, held until the reader
    /// leaves the tab.
    pub offer: Option<PairingOffer>,
    /// The window this runtime is advertising, re-read while the panel is open.
    pub window: Option<RemoteLanPairingWindowSnapshot>,
    pub error: Option<PairingError>,
}

impl Pairing {
    /// The request the reader is being asked about: the oldest one still
    /// waiting.
    ///
    /// The coordinator accepts one device per window, so a panel that showed
    /// every pending request at once would be offering a choice that does not
    /// exist. The oldest is the one the phone has been waiting on longest.
    pub fn pending(&self) -> Option<&vibex_core::RemoteLanPairingPendingRequestSummary> {
        self.window.as_ref().and_then(|window| {
            window
                .pending_requests
                .iter()
                .find(|request| request.state == RemoteLanPairingRequestState::Pending)
        })
    }

    /// Whether a request the reader made is still on its way.
    ///
    /// The open row draws a spinner for as long as this is true, and that
    /// spinner's frame comes from the animation phase: the clock has to keep
    /// stepping while the answer is out, or the row stands on one frame and the
    /// reader concludes the press went nowhere. The state is set only while a
    /// request is in flight and cleared by the answer — or by the panel closing,
    /// which is what keeps the clock from running for a surface nobody has open.
    pub fn is_busy(&self) -> bool {
        self.busy.is_some()
    }

    /// How many requests are waiting, including the one [`Self::pending`] names.
    pub fn waiting(&self) -> usize {
        self.window
            .as_ref()
            .map(|window| {
                window
                    .pending_requests
                    .iter()
                    .filter(|request| request.state == RemoteLanPairingRequestState::Pending)
                    .count()
            })
            .unwrap_or(0)
    }

    /// Whether the window has run out, which the panel says rather than
    /// pretending a dead advertisement is still up.
    pub fn expired(&self, now_ms: i64) -> bool {
        self.window
            .as_ref()
            .is_some_and(|window| window.discovery.expires_at_ms <= now_ms)
    }

    /// The offer, while the runtime would still accept it.
    pub fn live_offer(&self, now_ms: i64) -> Option<&PairingOffer> {
        self.offer
            .as_ref()
            .filter(|offer| offer.expires_at_ms > now_ms)
    }

    /// Whether the open row has nothing left to show: no offer, one the
    /// runtime has already stopped accepting, or a window that has closed.
    ///
    /// The press that finds nothing live is the press that asks again, which is
    /// why this is a question the reducer asks rather than a state the panel
    /// draws.
    pub fn spent(&self, method: PairMethod, now_ms: i64) -> bool {
        match method {
            PairMethod::LocalNetwork => self.window.is_none() || self.expired(now_ms),
            PairMethod::QrCode | PairMethod::CodeOrLink => self.live_offer(now_ms).is_none(),
        }
    }

    /// Everything the page learned from a fresh read or a finished action.
    pub fn settle(&mut self, window: Option<RemoteLanPairingWindowSnapshot>) {
        self.busy = None;
        self.error = None;
        self.window = window;
    }

    /// Files a failure under the row it came from and stops the spinner there.
    pub fn fail(&mut self, error: PairingError) {
        self.busy = None;
        self.error = Some(error);
    }

    /// Gives up everything held for the pairing tab: the offer is shown once,
    /// on this surface, and leaving it is what "once" means.
    pub fn clear(&mut self) {
        self.busy = None;
        self.offer = None;
        self.window = None;
        self.error = None;
    }
}

/// The page's own navigation: which tab is open, which row the keyboard is on,
/// and which pairing row has its panel open.
#[derive(Debug, Clone, Default)]
pub struct DeviceCenter {
    pub tab: DeviceTab,
    /// The row cursor, one per tab, so leaving a tab and coming back lands
    /// where the reader left rather than at the top.
    cursor: std::collections::BTreeMap<&'static str, usize>,
    /// The pairing row whose panel is open, if any. One at a time: the panels
    /// are built from the same window and the same offer, and two open at once
    /// would be the same facts drawn twice.
    pub open: Option<PairMethod>,
    pub pairing: Pairing,
}

impl DeviceCenter {
    pub fn cursor(&self, tab: DeviceTab) -> usize {
        self.cursor.get(tab.key()).copied().unwrap_or(0)
    }

    pub fn set_cursor(&mut self, tab: DeviceTab, row: usize) {
        self.cursor.insert(tab.key(), row);
    }

    /// How many rows the tab draws.
    pub fn row_count(&self, tab: DeviceTab, pair_rows: usize, devices: usize) -> usize {
        match tab {
            DeviceTab::Pair => pair_rows,
            DeviceTab::Devices => devices,
        }
    }

    /// The row the keyboard is on, clamped to the rows that exist.
    ///
    /// A page whose rows shrank under the cursor — a window that closed, a
    /// runtime that lost a capability — keeps the cursor on a row rather than
    /// on nothing.
    pub fn clamped_cursor(&self, pair_rows: usize, devices: usize) -> usize {
        let count = self.row_count(self.tab, pair_rows, devices);
        if count == 0 {
            return 0;
        }
        self.cursor(self.tab).min(count - 1)
    }

    /// Switch halves. Returns whether an advertisement has to come down: the
    /// window belongs to the pairing rows, so leaving that half closes the
    /// panels and gives up the offer — a code shown again after the reader
    /// walked away is shown twice.
    pub fn select_tab(&mut self, tab: DeviceTab) -> bool {
        if self.tab == tab {
            return false;
        }
        self.tab = tab;
        self.open = None;
        let cancel_window = self.pairing.window.is_some();
        self.pairing.clear();
        cancel_window
    }

    pub fn step_tab(&mut self, delta: i64) -> bool {
        let tabs = DeviceTab::ALL;
        let current = tabs.iter().position(|tab| *tab == self.tab).unwrap_or(0) as i64;
        let next = (current + delta).rem_euclid(tabs.len() as i64) as usize;
        self.select_tab(tabs[next])
    }

    /// Opens or closes one pairing row. The answer says what the press asks
    /// for, so the reducer owns the decision to perform I/O.
    pub fn toggle(&mut self, method: PairMethod) -> Activation {
        if self.open == Some(method) {
            self.open = None;
            return Activation::Closed {
                cancel_window: method == PairMethod::LocalNetwork,
            };
        }
        self.open = Some(method);
        Activation::Opened(method.opening())
    }

    /// Whether the panel behind the LAN row is up.
    pub fn lan_panel_open(&self) -> bool {
        self.tab == DeviceTab::Pair && self.open == Some(PairMethod::LocalNetwork)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const AVAILABLE: Availability = Availability::Available;
    const UNSUPPORTED: Availability = Availability::Unsupported;

    fn rows(kind: RemoteServerKind, pairing: Availability, lan: Availability) -> Vec<PairMethod> {
        pair_methods(kind, pairing, lan)
    }

    #[test]
    fn a_desktop_runtime_pairs_phones_and_a_headless_one_also_pairs_desktops() {
        assert_eq!(
            rows(RemoteServerKind::Desktop, AVAILABLE, AVAILABLE),
            vec![PairMethod::QrCode, PairMethod::LocalNetwork]
        );
        assert_eq!(
            rows(RemoteServerKind::Headless, AVAILABLE, AVAILABLE),
            vec![PairMethod::QrCode, PairMethod::CodeOrLink]
        );
    }

    #[test]
    fn a_runtime_without_lan_pairing_does_not_offer_the_row() {
        assert_eq!(
            rows(RemoteServerKind::Desktop, AVAILABLE, UNSUPPORTED),
            vec![PairMethod::QrCode]
        );
    }

    #[test]
    fn a_runtime_that_cannot_pair_offers_nothing() {
        assert!(rows(RemoteServerKind::Desktop, UNSUPPORTED, AVAILABLE).is_empty());
        // A peer that predates the kind still gets the offer: the QR is the one
        // route that never depended on knowing what the runtime is.
        assert_eq!(
            rows(RemoteServerKind::Unknown, AVAILABLE, AVAILABLE),
            vec![PairMethod::QrCode]
        );
        // And the local-network row is a desktop fact, so a peer of unknown
        // kind is not offered it on the strength of the capability alone.
        assert_eq!(
            rows(RemoteServerKind::Unknown, AVAILABLE, AVAILABLE),
            vec![PairMethod::QrCode]
        );
    }

    #[test]
    fn leaving_the_pairing_tab_closes_its_panel_and_drops_the_offer() {
        let mut center = DeviceCenter::default();
        assert_eq!(
            center.toggle(PairMethod::QrCode),
            Activation::Opened(Opening::Offer)
        );
        center.pairing.offer = Some(PairingOffer {
            code: "1234".to_string(),
            url: "vibex://open/direct#/pair/x".to_string(),
            permission: RemoteDevicePermissionLevel::FullControl,
            expires_at_ms: i64::MAX,
        });
        center.select_tab(DeviceTab::Devices);
        assert!(center.open.is_none());
        assert!(center.pairing.offer.is_none());
        center.select_tab(DeviceTab::Pair);
        assert_eq!(center.cursor(DeviceTab::Pair), 0);
    }

    #[test]
    fn closing_the_window_panel_asks_for_the_advertisement_to_come_down() {
        let mut center = DeviceCenter::default();
        assert_eq!(
            center.toggle(PairMethod::LocalNetwork),
            Activation::Opened(Opening::Window)
        );
        assert!(center.lan_panel_open());
        assert_eq!(
            center.toggle(PairMethod::LocalNetwork),
            Activation::Closed {
                cancel_window: true
            }
        );
        assert!(!center.lan_panel_open());
        // The QR panel is not an advertisement, so closing it asks for nothing.
        assert_eq!(
            center.toggle(PairMethod::QrCode),
            Activation::Opened(Opening::Offer)
        );
        assert_eq!(
            center.toggle(PairMethod::QrCode),
            Activation::Closed {
                cancel_window: false
            }
        );
    }

    #[test]
    fn the_pending_request_is_the_oldest_one_still_waiting() {
        let mut pairing = Pairing::default();
        assert!(pairing.pending().is_none());
        assert_eq!(pairing.waiting(), 0);
        pairing.window = Some(window(vec![
            ("older", RemoteLanPairingRequestState::Pending),
            ("newer", RemoteLanPairingRequestState::Pending),
            ("settled", RemoteLanPairingRequestState::Approved),
        ]));
        assert_eq!(
            pairing
                .pending()
                .map(|request| request.display_name.as_str()),
            Some("older")
        );
        assert_eq!(pairing.waiting(), 2);
    }

    fn window(
        requests: Vec<(&str, RemoteLanPairingRequestState)>,
    ) -> RemoteLanPairingWindowSnapshot {
        RemoteLanPairingWindowSnapshot {
            discovery: vibex_core::RemoteLanPairingDiscoverySummary {
                schema_version: "v1".to_string(),
                window_id: vibex_core::RequestId::new(),
                server_id: "server".to_string(),
                server_identity_public_key: "key".to_string(),
                protocol_range: vibex_core::RemoteProtocolVersionRange::v2(),
                permission_level: RemoteDevicePermissionLevel::FullControl,
                expires_at_ms: 1_000,
            },
            advertisement: vibex_core::RemoteLanPairingAdvertisement {
                advertisement_id: "ad".to_string(),
                service_instance: "instance".to_string(),
                display_name: "runtime".to_string(),
                direct_origin: "https://localhost:1428".to_string(),
                protocol_min: 2,
                protocol_max: 2,
            },
            pending_requests: requests
                .into_iter()
                .map(
                    |(name, state)| vibex_core::RemoteLanPairingPendingRequestSummary {
                        request_id: vibex_core::RequestId::new(),
                        display_name: name.to_string(),
                        device_fingerprint: "A1B2C3".to_string(),
                        verification_code: "4821".to_string(),
                        state,
                        expires_at_ms: 1_000,
                    },
                )
                .collect(),
        }
    }
}
