//! Vehicle-status plane: `VehicleStatusUpdate` (0xA101) typed parse + `StartVehicleStatusUpdates`
//! (0xA100) selectable-field subscribe model.
//!
//! DIRECTION (from Apple's first-party spec `source` of truth):
//! - `StartVehicleStatusUpdates` (0xA100) `source=1` (DEVICE→accessory): the iPhone SUBSCRIBES,
//!   listing — as presence-only params — which vehicle fields it wants pushed. The accessory (this
//!   codebase) PARSES it → [`parse_start_vehicle_status_updates`] yields a [`VehicleStatusSelection`].
//! - `VehicleStatusUpdate` (0xA101) `source=0` (ACCESSORY→device): the accessory reports the typed
//!   values for the selected fields. [`parse_vehicle_status_update`] decodes that body into a typed
//!   [`VehicleStatus`] (symmetric with the NowPlaying/RouteGuidance parse path; used for the wire
//!   consumer + round-trip tests).
//!
//! Param ids/types/enum tables are verbatim from Apple's `CarPlaySimulator.devicekitplugin` spec.

use crate::tlv::{boolv, i16v, parse_groups, u16v, u32v, u8v, utf8};
use serde::{Deserialize, Serialize};

/// Apple iAP2 parameter ids for the vehicle-status plane. Defined locally (NOT `use`d from
/// `carplay_iap2_core::spec`) because that crate depends on this one — importing it back would cycle.
/// Each const carries Apple's authoritative name.
mod pid {
    /// `VehicleStatusUpdate` (0xA101) / `StartVehicleStatusUpdates` (0xA100) flat param ids. The
    /// subscribe uses ids 3..=20 as presence-only selectors; the update carries these plus the EV
    /// routing fields 21..=30 as typed values.
    pub const RANGE: u16 = 3; // Range (uint16, km)
    pub const OUTSIDE_TEMPERATURE: u16 = 4; // OutsideTemperature (int16, degrees C)
    pub const INSIDE_TEMPERATURE: u16 = 5; // InsideTemperature (int16, degrees C)
    pub const RANGE_WARNING: u16 = 6; // RangeWarning (bool)
    pub const RANGE_GASOLINE: u16 = 9; // RangeGasoline (uint16, km)
    pub const RANGE_DIESEL: u16 = 10; // RangeDiesel (uint16, km)
    pub const RANGE_ELECTRIC: u16 = 11; // RangeElectric (uint16, km)
    pub const RANGE_CNG: u16 = 12; // RangeCNG (uint16, km)
    pub const RANGE_WARNING_GASOLINE: u16 = 13; // RangeWarningGasoline (bool)
    pub const RANGE_WARNING_DIESEL: u16 = 14; // RangeWarningDiesel (bool)
    pub const RANGE_WARNING_ELECTRIC: u16 = 15; // RangeWarningElectric (bool)
    pub const RANGE_WARNING_CNG: u16 = 16; // RangeWarningCNG (bool)
    pub const WIPER_STATUS: u16 = 17; // WiperStatus (group)
    pub const BAROMETRIC_PRESSURE: u16 = 18; // BarometricPressure (uint16, millibar)
    pub const ALERTS: u16 = 19; // Alerts (enum[0+] VehicleAlerts)
    pub const PASSENGER_SEAT_STATUS: u16 = 20; // PassengerSeatStatus (bool)
    pub const MIN_BATTERY_CHARGE: u16 = 21; // MinBatteryCharge (uint32, Wh)
    pub const CURRENT_BATTERY_CHARGE: u16 = 22; // CurrentBatteryCharge (uint32, Wh)
    pub const MAX_BATTERY_CHARGE: u16 = 23; // MaxBatteryCharge (uint32, Wh)
    pub const DISPLAYED_BATTERY_PERCENTAGE: u16 = 24; // DisplayedBatteryPercentage (uint32, %*1000)
    pub const IS_CHARGING: u16 = 25; // isCharging (bool)
    pub const CHARGING_PARAMETER: u16 = 26; // ChargingParameter (utf8, JSON)
    pub const CONSUMPTION_PARAMETER: u16 = 27; // ConsumptionParameter (utf8, JSON)
    pub const ACTIVE_CONNECTOR: u16 = 28; // ActiveConnector (enum ChargingConnectorTypes)
    pub const MAX_RANGE_ELECTRIC: u16 = 30; // MaxRangeElectric (uint16, km)

    /// `WiperStatus` (id 17) group sub-param ids.
    pub mod wiper {
        pub const WASHER_ON: u16 = 0; // WasherOn (bool)
        pub const WIPE_DURATION: u16 = 1; // WipeDuration (msecs16)
        pub const WAIT_DURATION: u16 = 2; // WaitDuration (msecs32)
    }
}

/// `VehicleAlerts` — Apple `VehicleStatusUpdate` (0xA101) param `19` **Alerts** enum table. The wire
/// value is an `enum[0+]` (a list), so an update can raise several alerts at once.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum VehicleAlert {
    /// Value 0 — "Traction loss".
    TractionLoss,
    /// Value 1 — "Anti-lock brake system warning".
    AntiLockBrakeSystem,
    /// Value 2 — "Low tire pressure warning".
    LowTirePressure,
    /// Value 3 — "Hazard warning".
    Hazard,
    /// Value 4 — "Hard brake".
    HardBrake,
    /// Value 5 — "Air bag deploy".
    AirBagDeploy,
}

impl VehicleAlert {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => VehicleAlert::TractionLoss,
            1 => VehicleAlert::AntiLockBrakeSystem,
            2 => VehicleAlert::LowTirePressure,
            3 => VehicleAlert::Hazard,
            4 => VehicleAlert::HardBrake,
            5 => VehicleAlert::AirBagDeploy,
            _ => return None,
        })
    }
}

/// `ChargingConnectorTypes` — Apple `VehicleStatusUpdate` (0xA101) param `28` **ActiveConnector**
/// enum table (the connector active while charging).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChargingConnectorType {
    Ccs1,
    Ccs2,
    J1772,
    ChaDeMo,
    Mennekes,
    /// "GB/T (DC)".
    GbtDc,
    /// "GB/T (AC)".
    GbtAc,
    /// "NACS (DC)".
    NacsDc,
    /// "NACS (AC)".
    NacsAc,
}

impl ChargingConnectorType {
    pub fn from_u8(v: u8) -> Option<Self> {
        use ChargingConnectorType::*;
        Some(match v {
            0 => Ccs1,
            1 => Ccs2,
            2 => J1772,
            3 => ChaDeMo,
            4 => Mennekes,
            5 => GbtDc,
            6 => GbtAc,
            7 => NacsDc,
            8 => NacsAc,
            _ => return None,
        })
    }
}

/// `WiperStatus` (0xA101 param `17`) group. Sub-params `WasherOn`(0, bool),
/// `WipeDuration`(1, msecs16), `WaitDuration`(2, msecs32).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WiperStatus {
    /// `WasherOn` (id 0): washer fluid is spraying.
    pub washer_on: Option<bool>,
    /// `WipeDuration` (id 1), milliseconds — the wipe stroke duration.
    pub wipe_duration_ms: Option<u16>,
    /// `WaitDuration` (id 2), milliseconds — the wait between wipes.
    pub wait_duration_ms: Option<u32>,
}

impl WiperStatus {
    pub fn merge(&mut self, delta: WiperStatus) {
        if delta.washer_on.is_some() {
            self.washer_on = delta.washer_on;
        }
        if delta.wipe_duration_ms.is_some() {
            self.wipe_duration_ms = delta.wipe_duration_ms;
        }
        if delta.wait_duration_ms.is_some() {
            self.wait_duration_ms = delta.wait_duration_ms;
        }
    }
}

/// Vehicle-status snapshot. Field names follow Apple's `VehicleStatusUpdate` (0xA101) params.
/// All fields `Option`: like NowPlaying, the accessory sends deltas carrying only changed fields, so
/// `None` means "not present in this update", not a real value — merge into a running snapshot.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct VehicleStatus {
    /// `Range` (id 3), km — remaining total vehicle range.
    pub range_km: Option<u16>,
    /// `OutsideTemperature` (id 4), degrees C — SIGNED (can be below zero).
    pub outside_temperature_c: Option<i16>,
    /// `InsideTemperature` (id 5), degrees C — SIGNED.
    pub inside_temperature_c: Option<i16>,
    /// `RangeWarning` (id 6): low-range indicator set.
    pub range_warning: Option<bool>,
    /// `RangeGasoline` (id 9), km.
    pub range_gasoline_km: Option<u16>,
    /// `RangeDiesel` (id 10), km.
    pub range_diesel_km: Option<u16>,
    /// `RangeElectric` (id 11), km.
    pub range_electric_km: Option<u16>,
    /// `RangeCNG` (id 12), km.
    pub range_cng_km: Option<u16>,
    /// `RangeWarningGasoline` (id 13).
    pub range_warning_gasoline: Option<bool>,
    /// `RangeWarningDiesel` (id 14).
    pub range_warning_diesel: Option<bool>,
    /// `RangeWarningElectric` (id 15).
    pub range_warning_electric: Option<bool>,
    /// `RangeWarningCNG` (id 16).
    pub range_warning_cng: Option<bool>,
    /// `WiperStatus` (id 17): washer/wipe/wait sub-group.
    pub wiper_status: Option<WiperStatus>,
    /// `BarometricPressure` (id 18), millibar.
    pub barometric_pressure_mbar: Option<u16>,
    /// `Alerts` (id 19): zero or more active `VehicleAlerts`.
    pub alerts: Option<Vec<VehicleAlert>>,
    /// `PassengerSeatStatus` (id 20): passenger seat occupied.
    pub passenger_seat_occupied: Option<bool>,
    /// `MinBatteryCharge` (id 21), Wh — EV-routing displayed minimum.
    pub min_battery_charge_wh: Option<u32>,
    /// `CurrentBatteryCharge` (id 22), Wh — EV-routing current charge.
    pub current_battery_charge_wh: Option<u32>,
    /// `MaxBatteryCharge` (id 23), Wh — EV-routing full-charge capacity.
    pub max_battery_charge_wh: Option<u32>,
    /// `DisplayedBatteryPercentage` (id 24): percent *1000 (51.237 % = 51237).
    pub displayed_battery_percentage_milli: Option<u32>,
    /// `isCharging` (id 25).
    pub is_charging: Option<bool>,
    /// `ChargingParameter` (id 26): JSON string, EV charging model.
    pub charging_parameter: Option<String>,
    /// `ConsumptionParameter` (id 27): JSON string, EV consumption model.
    pub consumption_parameter: Option<String>,
    /// `ActiveConnector` (id 28): connector in use while charging.
    pub active_connector: Option<ChargingConnectorType>,
    /// `MaxRangeElectric` (id 30), km — max electric range at full charge.
    pub max_range_electric_km: Option<u16>,
}

impl VehicleStatus {
    /// Merge a delta into `self`: overwrite only fields the delta actually carried, mirroring
    /// `NowPlaying::merge`. Nested `WiperStatus` is merged field-wise so a partial wiper update does
    /// not blank out the other wiper sub-fields.
    pub fn merge(&mut self, delta: VehicleStatus) {
        if delta.range_km.is_some() {
            self.range_km = delta.range_km;
        }
        if delta.outside_temperature_c.is_some() {
            self.outside_temperature_c = delta.outside_temperature_c;
        }
        if delta.inside_temperature_c.is_some() {
            self.inside_temperature_c = delta.inside_temperature_c;
        }
        if delta.range_warning.is_some() {
            self.range_warning = delta.range_warning;
        }
        if delta.range_gasoline_km.is_some() {
            self.range_gasoline_km = delta.range_gasoline_km;
        }
        if delta.range_diesel_km.is_some() {
            self.range_diesel_km = delta.range_diesel_km;
        }
        if delta.range_electric_km.is_some() {
            self.range_electric_km = delta.range_electric_km;
        }
        if delta.range_cng_km.is_some() {
            self.range_cng_km = delta.range_cng_km;
        }
        if delta.range_warning_gasoline.is_some() {
            self.range_warning_gasoline = delta.range_warning_gasoline;
        }
        if delta.range_warning_diesel.is_some() {
            self.range_warning_diesel = delta.range_warning_diesel;
        }
        if delta.range_warning_electric.is_some() {
            self.range_warning_electric = delta.range_warning_electric;
        }
        if delta.range_warning_cng.is_some() {
            self.range_warning_cng = delta.range_warning_cng;
        }
        if let Some(w) = delta.wiper_status {
            self.wiper_status.get_or_insert_with(WiperStatus::default).merge(w);
        }
        if delta.barometric_pressure_mbar.is_some() {
            self.barometric_pressure_mbar = delta.barometric_pressure_mbar;
        }
        if delta.alerts.is_some() {
            self.alerts = delta.alerts;
        }
        if delta.passenger_seat_occupied.is_some() {
            self.passenger_seat_occupied = delta.passenger_seat_occupied;
        }
        if delta.min_battery_charge_wh.is_some() {
            self.min_battery_charge_wh = delta.min_battery_charge_wh;
        }
        if delta.current_battery_charge_wh.is_some() {
            self.current_battery_charge_wh = delta.current_battery_charge_wh;
        }
        if delta.max_battery_charge_wh.is_some() {
            self.max_battery_charge_wh = delta.max_battery_charge_wh;
        }
        if delta.displayed_battery_percentage_milli.is_some() {
            self.displayed_battery_percentage_milli = delta.displayed_battery_percentage_milli;
        }
        if delta.is_charging.is_some() {
            self.is_charging = delta.is_charging;
        }
        if delta.charging_parameter.is_some() {
            self.charging_parameter = delta.charging_parameter;
        }
        if delta.consumption_parameter.is_some() {
            self.consumption_parameter = delta.consumption_parameter;
        }
        if delta.active_connector.is_some() {
            self.active_connector = delta.active_connector;
        }
        if delta.max_range_electric_km.is_some() {
            self.max_range_electric_km = delta.max_range_electric_km;
        }
    }
}

/// Which vehicle-status fields the device requested, decoded from a `StartVehicleStatusUpdates`
/// (0xA100) body. Every listed param there is presence-only (`type=none`, zero-length value) and
/// merely SELECTS a field; there is no value to read. Ids 3..=20 are selectable (the EV-routing
/// fields 21..=30 are not part of the subscribe selection).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VehicleStatusSelection {
    pub range: bool,
    pub outside_temperature: bool,
    pub inside_temperature: bool,
    pub range_warning: bool,
    pub range_gasoline: bool,
    pub range_diesel: bool,
    pub range_electric: bool,
    pub range_cng: bool,
    pub range_warning_gasoline: bool,
    pub range_warning_diesel: bool,
    pub range_warning_electric: bool,
    pub range_warning_cng: bool,
    pub wiper_status: bool,
    pub barometric_pressure: bool,
    pub alerts: bool,
    pub passenger_seat_status: bool,
}

/// Parse a `StartVehicleStatusUpdates` (0xA100, device-source) body into the set of selected fields.
/// The body is a flat sequence of presence-only param groups; we key on the param id alone.
pub fn parse_start_vehicle_status_updates(body: &[u8]) -> VehicleStatusSelection {
    let mut sel = VehicleStatusSelection::default();
    for (id, _val) in parse_groups(body) {
        match id {
            pid::RANGE => sel.range = true,
            pid::OUTSIDE_TEMPERATURE => sel.outside_temperature = true,
            pid::INSIDE_TEMPERATURE => sel.inside_temperature = true,
            pid::RANGE_WARNING => sel.range_warning = true,
            pid::RANGE_GASOLINE => sel.range_gasoline = true,
            pid::RANGE_DIESEL => sel.range_diesel = true,
            pid::RANGE_ELECTRIC => sel.range_electric = true,
            pid::RANGE_CNG => sel.range_cng = true,
            pid::RANGE_WARNING_GASOLINE => sel.range_warning_gasoline = true,
            pid::RANGE_WARNING_DIESEL => sel.range_warning_diesel = true,
            pid::RANGE_WARNING_ELECTRIC => sel.range_warning_electric = true,
            pid::RANGE_WARNING_CNG => sel.range_warning_cng = true,
            pid::WIPER_STATUS => sel.wiper_status = true,
            pid::BAROMETRIC_PRESSURE => sel.barometric_pressure = true,
            pid::ALERTS => sel.alerts = true,
            pid::PASSENGER_SEAT_STATUS => sel.passenger_seat_status = true,
            _ => {}
        }
    }
    sel
}

/// Parse a `VehicleStatusUpdate` (0xA101, accessory-source) body: FLAT params (with `WiperStatus`
/// nesting one sub-group). Param ids/types validated verbatim against Apple's spec.
///
/// `Alerts` (id 19) is an `enum[0+]` list. Apple's decoded spec does not pin the per-element width;
/// consistent with every other enum in this crate (`PlaybackStatus`, `DistanceUnit`) we decode one
/// byte per alert. Flagged for on-hardware confirmation.
pub fn parse_vehicle_status_update(body: &[u8]) -> VehicleStatus {
    let mut v = VehicleStatus::default();
    for (id, val) in parse_groups(body) {
        match id {
            pid::RANGE => v.range_km = u16v(val),
            pid::OUTSIDE_TEMPERATURE => v.outside_temperature_c = i16v(val),
            pid::INSIDE_TEMPERATURE => v.inside_temperature_c = i16v(val),
            pid::RANGE_WARNING => v.range_warning = boolv(val),
            pid::RANGE_GASOLINE => v.range_gasoline_km = u16v(val),
            pid::RANGE_DIESEL => v.range_diesel_km = u16v(val),
            pid::RANGE_ELECTRIC => v.range_electric_km = u16v(val),
            pid::RANGE_CNG => v.range_cng_km = u16v(val),
            pid::RANGE_WARNING_GASOLINE => v.range_warning_gasoline = boolv(val),
            pid::RANGE_WARNING_DIESEL => v.range_warning_diesel = boolv(val),
            pid::RANGE_WARNING_ELECTRIC => v.range_warning_electric = boolv(val),
            pid::RANGE_WARNING_CNG => v.range_warning_cng = boolv(val),
            pid::WIPER_STATUS => v.wiper_status = Some(parse_wiper_status(val)),
            pid::BAROMETRIC_PRESSURE => v.barometric_pressure_mbar = u16v(val),
            pid::ALERTS => {
                v.alerts = Some(val.iter().filter_map(|&b| VehicleAlert::from_u8(b)).collect())
            }
            pid::PASSENGER_SEAT_STATUS => v.passenger_seat_occupied = boolv(val),
            pid::MIN_BATTERY_CHARGE => v.min_battery_charge_wh = u32v(val),
            pid::CURRENT_BATTERY_CHARGE => v.current_battery_charge_wh = u32v(val),
            pid::MAX_BATTERY_CHARGE => v.max_battery_charge_wh = u32v(val),
            pid::DISPLAYED_BATTERY_PERCENTAGE => v.displayed_battery_percentage_milli = u32v(val),
            pid::IS_CHARGING => v.is_charging = boolv(val),
            pid::CHARGING_PARAMETER => v.charging_parameter = utf8(val),
            pid::CONSUMPTION_PARAMETER => v.consumption_parameter = utf8(val),
            pid::ACTIVE_CONNECTOR => {
                v.active_connector = u8v(val).and_then(ChargingConnectorType::from_u8)
            }
            pid::MAX_RANGE_ELECTRIC => v.max_range_electric_km = u16v(val),
            _ => {}
        }
    }
    v
}

/// Parse the nested `WiperStatus` (id 17) sub-group value.
fn parse_wiper_status(val: &[u8]) -> WiperStatus {
    let mut w = WiperStatus::default();
    for (id, sub) in parse_groups(val) {
        match id {
            pid::wiper::WASHER_ON => w.washer_on = boolv(sub),
            pid::wiper::WIPE_DURATION => w.wipe_duration_ms = u16v(sub),
            pid::wiper::WAIT_DURATION => w.wait_duration_ms = u32v(sub),
            _ => {}
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `[len:BE16 (incl. 4-byte header)][pid:BE16][value...]` — the same wire framing the rest of the
    /// crate uses, so fixtures are spec-derived rather than hand-counted.
    fn group(pid: u16, value: &[u8]) -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(&((4 + value.len()) as u16).to_be_bytes());
        g.extend_from_slice(&pid.to_be_bytes());
        g.extend_from_slice(value);
        g
    }

    fn str_val(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    #[test]
    fn parse_subscribe_selects_only_listed_presence_only_fields() {
        // Device requests Range(3), OutsideTemperature(4), RangeWarning(6), WiperStatus(17),
        // Alerts(19). Presence-only => zero-length values.
        let body = [
            group(pid::RANGE, &[]),
            group(pid::OUTSIDE_TEMPERATURE, &[]),
            group(pid::RANGE_WARNING, &[]),
            group(pid::WIPER_STATUS, &[]),
            group(pid::ALERTS, &[]),
        ]
        .concat();
        let sel = parse_start_vehicle_status_updates(&body);
        assert!(sel.range && sel.outside_temperature && sel.range_warning);
        assert!(sel.wiper_status && sel.alerts);
        // Not requested => stay false.
        assert!(!sel.inside_temperature && !sel.barometric_pressure && !sel.passenger_seat_status);
    }

    #[test]
    fn parse_update_typed_scalars_including_signed_temperatures() {
        let body = [
            group(pid::RANGE, &420u16.to_be_bytes()),
            group(pid::OUTSIDE_TEMPERATURE, &(-7i16).to_be_bytes()), // below freezing
            group(pid::INSIDE_TEMPERATURE, &21i16.to_be_bytes()),
            group(pid::RANGE_WARNING, &[1]),
            group(pid::BAROMETRIC_PRESSURE, &1013u16.to_be_bytes()),
            group(pid::PASSENGER_SEAT_STATUS, &[0]),
        ]
        .concat();
        let v = parse_vehicle_status_update(&body);
        assert_eq!(v.range_km, Some(420));
        assert_eq!(v.outside_temperature_c, Some(-7));
        assert_eq!(v.inside_temperature_c, Some(21));
        assert_eq!(v.range_warning, Some(true));
        assert_eq!(v.barometric_pressure_mbar, Some(1013));
        assert_eq!(v.passenger_seat_occupied, Some(false));
    }

    #[test]
    fn parse_update_wiper_status_nested_group() {
        let inner = [
            group(pid::wiper::WASHER_ON, &[1]),
            group(pid::wiper::WIPE_DURATION, &350u16.to_be_bytes()),
            group(pid::wiper::WAIT_DURATION, &2000u32.to_be_bytes()),
        ]
        .concat();
        let body = group(pid::WIPER_STATUS, &inner);
        let v = parse_vehicle_status_update(&body);
        let w = v.wiper_status.expect("wiper group present");
        assert_eq!(w.washer_on, Some(true));
        assert_eq!(w.wipe_duration_ms, Some(350));
        assert_eq!(w.wait_duration_ms, Some(2000));
    }

    #[test]
    fn parse_update_alerts_enum_list() {
        // Two alerts: LowTirePressure(2) and Hazard(3); an out-of-table value (99) is dropped.
        let body = group(pid::ALERTS, &[2, 3, 99]);
        let v = parse_vehicle_status_update(&body);
        assert_eq!(
            v.alerts,
            Some(vec![VehicleAlert::LowTirePressure, VehicleAlert::Hazard])
        );
    }

    #[test]
    fn parse_update_ev_routing_fields() {
        let body = [
            group(pid::CURRENT_BATTERY_CHARGE, &42_000u32.to_be_bytes()),
            group(pid::MAX_BATTERY_CHARGE, &75_000u32.to_be_bytes()),
            group(pid::DISPLAYED_BATTERY_PERCENTAGE, &51_237u32.to_be_bytes()),
            group(pid::IS_CHARGING, &[1]),
            group(pid::ACTIVE_CONNECTOR, &[7]), // NACS (DC)
            group(pid::MAX_RANGE_ELECTRIC, &480u16.to_be_bytes()),
            group(pid::CHARGING_PARAMETER, &str_val("{\"model_id\":\"x\"}")),
        ]
        .concat();
        let v = parse_vehicle_status_update(&body);
        assert_eq!(v.current_battery_charge_wh, Some(42_000));
        assert_eq!(v.max_battery_charge_wh, Some(75_000));
        assert_eq!(v.displayed_battery_percentage_milli, Some(51_237));
        assert_eq!(v.is_charging, Some(true));
        assert_eq!(v.active_connector, Some(ChargingConnectorType::NacsDc));
        assert_eq!(v.max_range_electric_km, Some(480));
        assert_eq!(v.charging_parameter.as_deref(), Some("{\"model_id\":\"x\"}"));
    }

    #[test]
    fn merge_preserves_unlisted_fields_and_merges_wiper_subfields() {
        let mut snap = VehicleStatus {
            range_km: Some(400),
            outside_temperature_c: Some(10),
            wiper_status: Some(WiperStatus {
                washer_on: Some(false),
                wipe_duration_ms: Some(300),
                wait_duration_ms: Some(1000),
            }),
            ..Default::default()
        };
        // Delta touches only range + one wiper sub-field.
        let delta = VehicleStatus {
            range_km: Some(390),
            wiper_status: Some(WiperStatus { washer_on: Some(true), ..Default::default() }),
            ..Default::default()
        };
        snap.merge(delta);
        assert_eq!(snap.range_km, Some(390));
        assert_eq!(snap.outside_temperature_c, Some(10)); // untouched
        let w = snap.wiper_status.unwrap();
        assert_eq!(w.washer_on, Some(true)); // overwritten
        assert_eq!(w.wipe_duration_ms, Some(300)); // preserved through nested merge
        assert_eq!(w.wait_duration_ms, Some(1000));
    }

    #[test]
    fn connector_and_alert_enums_reject_out_of_table_values() {
        assert_eq!(ChargingConnectorType::from_u8(8), Some(ChargingConnectorType::NacsAc));
        assert_eq!(ChargingConnectorType::from_u8(9), None);
        assert_eq!(VehicleAlert::from_u8(5), Some(VehicleAlert::AirBagDeploy));
        assert_eq!(VehicleAlert::from_u8(6), None);
    }
}
