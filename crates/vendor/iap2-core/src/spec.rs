//! spec.rs — AUTHORITATIVE iAP2 / MFi4 message catalog, generated verbatim from Apple's
//! internal spec archives (`iap2messages-internal.i2mspecarchive`,
//! `mfi4authmessages-internal.i2mspecarchive`) shipped in Xcode 27's
//! CarPlaySimulator.devicekitplugin (macosx27.0 internal SDK). DO NOT hand-edit the generated
//! constants — they are Apple ground truth. This is the single source of protocol truth the
//! rest of the crate (and the wired/wireless drivers) should reference instead of bare magic
//! numbers. `Source` encodes Apple's `source` field: who ORIGINATES the message.

/// Which endpoint originates a message, per Apple's spec `source` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Sent BY the accessory (head unit) → belongs in IdentificationInformation param 6
    /// (MessagesSentByAccessory); the accessory must be able to build and send it.
    Accessory,
    /// Sent by the DEVICE (iPhone) → belongs in param 7 (MessagesReceivedFromDevice); the
    /// accessory must be able to parse and handle it.
    Device,
    /// MFi4 authentication-plane source contexts (Apple encodes these as 81..=84).
    Mfi(u8),
}

// ===== iAP2 control-session messages (144 messages) =====
/// `0x00B0` StartOOBBTPairing — Device, 0 params.
pub const MSG_START_OOBBT_PAIRING: u16 = 0x00B0;
/// `0x00B1` OOBBTPairingAccessoryInformation — Accessory, 2 params.
pub const MSG_OOBBT_PAIRING_ACCESSORY_INFORMATION: u16 = 0x00B1;
/// `0x00B2` OOBBTPairingLinkKeyInformation — Device, 2 params.
pub const MSG_OOBBT_PAIRING_LINK_KEY_INFORMATION: u16 = 0x00B2;
/// `0x00B3` OOBBTPairingCompletionInformation — Accessory, 1 params.
pub const MSG_OOBBT_PAIRING_COMPLETION_INFORMATION: u16 = 0x00B3;
/// `0x00B4` StopOOBBTPairing — Device, 0 params.
pub const MSG_STOP_OOBBT_PAIRING: u16 = 0x00B4;
/// `0x0B00` StartBluetoothPairing — Device, 2 params.
pub const MSG_START_BLUETOOTH_PAIRING: u16 = 0x0B00;
/// `0x0B01` BluetoothPairingAccessoryInformation — Accessory, 3 params.
pub const MSG_BLUETOOTH_PAIRING_ACCESSORY_INFORMATION: u16 = 0x0B01;
/// `0x0B02` BluetoothPairingStatus — Accessory, 3 params.
pub const MSG_BLUETOOTH_PAIRING_STATUS: u16 = 0x0B02;
/// `0x0B03` StopBluetoothPairing — Device, 4 params.
pub const MSG_STOP_BLUETOOTH_PAIRING: u16 = 0x0B03;
/// `0x0D00` StartRoadObjectDetectionUpdates — Device, 4 params.
pub const MSG_START_ROAD_OBJECT_DETECTION_UPDATES: u16 = 0x0D00;
/// `0x0D01` RoadObjectDetectionUpdate — Accessory, 7 params.
pub const MSG_ROAD_OBJECT_DETECTION_UPDATE: u16 = 0x0D01;
/// `0x0D02` StopRoadObjectDetectionUpdates — Device, 1 params.
pub const MSG_STOP_ROAD_OBJECT_DETECTION_UPDATES: u16 = 0x0D02;
/// `0x10DA` StartDestinationInformation — Accessory, 1 params.
pub const MSG_START_DESTINATION_INFORMATION: u16 = 0x10DA;
/// `0x10DB` DestinationInformation — Device, 8 params.
pub const MSG_DESTINATION_INFORMATION: u16 = 0x10DB;
/// `0x10DC` StopDestinationInformation — Accessory, 0 params.
pub const MSG_STOP_DESTINATION_INFORMATION: u16 = 0x10DC;
/// `0x10DD` DestinationInformationStatus — Accessory, 3 params.
pub const MSG_DESTINATION_INFORMATION_STATUS: u16 = 0x10DD;
/// `0x1D00` StartIdentification — Device, 0 params.
pub const MSG_START_IDENTIFICATION: u16 = 0x1D00;
/// `0x1D01` IdentificationInformation — Accessory, 35 params.
pub const MSG_IDENTIFICATION_INFORMATION: u16 = 0x1D01;
/// `0x1D02` IdentificationAccepted — Device, 0 params.
pub const MSG_IDENTIFICATION_ACCEPTED: u16 = 0x1D02;
/// `0x1D03` IdentificationRejected — Device, 31 params.
pub const MSG_IDENTIFICATION_REJECTED: u16 = 0x1D03;
/// `0x1D05` CancelIdentification — Accessory, 0 params.
pub const MSG_CANCEL_IDENTIFICATION: u16 = 0x1D05;
/// `0x1D06` IdentificationInformationUpdate — Accessory, 7 params.
pub const MSG_IDENTIFICATION_INFORMATION_UPDATE: u16 = 0x1D06;
/// `0x4154` StartCallStateUpdates — Accessory, 12 params.
pub const MSG_START_CALL_STATE_UPDATES: u16 = 0x4154;
/// `0x4155` CallStateUpdate — Device, 12 params.
pub const MSG_CALL_STATE_UPDATE: u16 = 0x4155;
/// `0x4156` StopCallStateUpdates — Accessory, 0 params.
pub const MSG_STOP_CALL_STATE_UPDATES: u16 = 0x4156;
/// `0x4157` StartCommunicationsUpdates — Accessory, 17 params.
pub const MSG_START_COMMUNICATIONS_UPDATES: u16 = 0x4157;
/// `0x4158` CommunicationsUpdate — Device, 17 params.
pub const MSG_COMMUNICATIONS_UPDATE: u16 = 0x4158;
/// `0x4159` StopCommunicationsUpdates — Accessory, 0 params.
pub const MSG_STOP_COMMUNICATIONS_UPDATES: u16 = 0x4159;
/// `0x415A` InitiateCall — Accessory, 4 params.
pub const MSG_INITIATE_CALL: u16 = 0x415A;
/// `0x415B` AcceptCall — Accessory, 2 params.
pub const MSG_ACCEPT_CALL: u16 = 0x415B;
/// `0x415C` EndCall — Accessory, 2 params.
pub const MSG_END_CALL: u16 = 0x415C;
/// `0x415D` SwapCalls — Accessory, 0 params.
pub const MSG_SWAP_CALLS: u16 = 0x415D;
/// `0x415E` MergeCalls — Accessory, 0 params.
pub const MSG_MERGE_CALLS: u16 = 0x415E;
/// `0x415F` HoldStatusUpdate — Accessory, 2 params.
pub const MSG_HOLD_STATUS_UPDATE: u16 = 0x415F;
/// `0x4160` MuteStatusUpdate — Accessory, 1 params.
pub const MSG_MUTE_STATUS_UPDATE: u16 = 0x4160;
/// `0x4161` SendDTMF — Accessory, 2 params.
pub const MSG_SEND_DTMF: u16 = 0x4161;
/// `0x4170` StartListUpdates — Accessory, 5 params.
pub const MSG_START_LIST_UPDATES: u16 = 0x4170;
/// `0x4171` ListUpdate — Device, 6 params.
pub const MSG_LIST_UPDATE: u16 = 0x4171;
/// `0x4172` StopListUpdates — Accessory, 0 params.
pub const MSG_STOP_LIST_UPDATES: u16 = 0x4172;
/// `0x4300` CarPlayAvailability — Device, 3 params.
pub const MSG_CAR_PLAY_AVAILABILITY: u16 = 0x4300;
/// `0x4301` CarPlayStartSession — Accessory, 9 params.
pub const MSG_CAR_PLAY_START_SESSION: u16 = 0x4301;
/// `0x4302` AvailableDigitalCarKeys — Accessory, 1 params.
pub const MSG_AVAILABLE_DIGITAL_CAR_KEYS: u16 = 0x4302;
/// `0x4303` MatchedDigitalCarKeys — Device, 1 params.
pub const MSG_MATCHED_DIGITAL_CAR_KEYS: u16 = 0x4303;
/// `0x4C00` StartMediaLibraryInformation — Accessory, 0 params.
pub const MSG_START_MEDIA_LIBRARY_INFORMATION: u16 = 0x4C00;
/// `0x4C01` MediaLibraryInformation — Device, 1 params.
pub const MSG_MEDIA_LIBRARY_INFORMATION: u16 = 0x4C01;
/// `0x4C02` StopMediaLibraryInformation — Accessory, 0 params.
pub const MSG_STOP_MEDIA_LIBRARY_INFORMATION: u16 = 0x4C02;
/// `0x4C03` StartMediaLibraryUpdates — Accessory, 8 params.
pub const MSG_START_MEDIA_LIBRARY_UPDATES: u16 = 0x4C03;
/// `0x4C04` MediaLibraryUpdate — Device, 10 params.
pub const MSG_MEDIA_LIBRARY_UPDATE: u16 = 0x4C04;
/// `0x4C05` StopMediaLibraryUpdates — Accessory, 1 params.
pub const MSG_STOP_MEDIA_LIBRARY_UPDATES: u16 = 0x4C05;
/// `0x4C06` PlayMediaLibraryCurrentSelection — Accessory, 1 params.
pub const MSG_PLAY_MEDIA_LIBRARY_CURRENT_SELECTION: u16 = 0x4C06;
/// `0x4C07` PlayMediaLibraryItems — Accessory, 3 params.
pub const MSG_PLAY_MEDIA_LIBRARY_ITEMS: u16 = 0x4C07;
/// `0x4C08` PlayMediaLibraryCollection — Accessory, 5 params.
pub const MSG_PLAY_MEDIA_LIBRARY_COLLECTION: u16 = 0x4C08;
/// `0x4C09` PlayMediaLibrarySpecial — Accessory, 3 params.
pub const MSG_PLAY_MEDIA_LIBRARY_SPECIAL: u16 = 0x4C09;
/// `0x4E01` BluetoothComponentInformation — Accessory, 1 params.
pub const MSG_BLUETOOTH_COMPONENT_INFORMATION: u16 = 0x4E01;
/// `0x4E03` StartBluetoothConnectionUpdates — Accessory, 1 params.
pub const MSG_START_BLUETOOTH_CONNECTION_UPDATES: u16 = 0x4E03;
/// `0x4E04` BluetoothConnectionUpdate — Device, 2 params.
pub const MSG_BLUETOOTH_CONNECTION_UPDATE: u16 = 0x4E04;
/// `0x4E05` StopBluetoothConnectionUpdates — Accessory, 0 params.
pub const MSG_STOP_BLUETOOTH_CONNECTION_UPDATES: u16 = 0x4E05;
/// `0x4E09` DeviceInformationUpdate — Device, 1 params.
pub const MSG_DEVICE_INFORMATION_UPDATE: u16 = 0x4E09;
/// `0x4E0A` DeviceLanguageUpdate — Device, 1 params.
pub const MSG_DEVICE_LANGUAGE_UPDATE: u16 = 0x4E0A;
/// `0x4E0B` DeviceTimeUpdate — Device, 3 params.
pub const MSG_DEVICE_TIME_UPDATE: u16 = 0x4E0B;
/// `0x4E0C` DeviceUUIDUpdate — Device, 1 params.
pub const MSG_DEVICE_UUID_UPDATE: u16 = 0x4E0C;
/// `0x4E0D` WirelessCarPlayUpdate — Device, 1 params.
pub const MSG_WIRELESS_CAR_PLAY_UPDATE: u16 = 0x4E0D;
/// `0x4E0E` DeviceTransportIdentifierNotification — Device, 2 params.
pub const MSG_DEVICE_TRANSPORT_IDENTIFIER_NOTIFICATION: u16 = 0x4E0E;
/// `0x5000` StartNowPlayingUpdates — Accessory, 3 params.
pub const MSG_START_NOW_PLAYING_UPDATES: u16 = 0x5000;
/// `0x5001` NowPlayingUpdate — Device, 2 params.
pub const MSG_NOW_PLAYING_UPDATE: u16 = 0x5001;
/// `0x5002` StopNowPlayingUpdates — Accessory, 0 params.
pub const MSG_STOP_NOW_PLAYING_UPDATES: u16 = 0x5002;
/// `0x5003` SetNowPlayingInformation — Accessory, 3 params.
pub const MSG_SET_NOW_PLAYING_INFORMATION: u16 = 0x5003;
/// `0x5200` StartRouteGuidanceUpdates — Accessory, 6 params.
pub const MSG_START_ROUTE_GUIDANCE_UPDATES: u16 = 0x5200;
/// `0x5201` RouteGuidanceUpdate — Device, 27 params.
pub const MSG_ROUTE_GUIDANCE_UPDATE: u16 = 0x5201;
/// `0x5202` RouteGuidanceManeuverInformation — Device, 14 params.
pub const MSG_ROUTE_GUIDANCE_MANEUVER_INFORMATION: u16 = 0x5202;
/// `0x5203` StopRouteGuidanceUpdates — Accessory, 1 params.
pub const MSG_STOP_ROUTE_GUIDANCE_UPDATES: u16 = 0x5203;
/// `0x5204` LaneGuidanceInformation — Device, 4 params.
pub const MSG_LANE_GUIDANCE_INFORMATION: u16 = 0x5204;
/// `0x5400` StartAssistiveTouch — Accessory, 0 params.
pub const MSG_START_ASSISTIVE_TOUCH: u16 = 0x5400;
/// `0x5401` StopAssistiveTouch — Accessory, 0 params.
pub const MSG_STOP_ASSISTIVE_TOUCH: u16 = 0x5401;
/// `0x5402` StartAssistiveTouchInformation — Accessory, 0 params.
pub const MSG_START_ASSISTIVE_TOUCH_INFORMATION: u16 = 0x5402;
/// `0x5403` AssistiveTouchInformation — Device, 1 params.
pub const MSG_ASSISTIVE_TOUCH_INFORMATION: u16 = 0x5403;
/// `0x5404` StopAssistiveTouchInformation — Accessory, 0 params.
pub const MSG_STOP_ASSISTIVE_TOUCH_INFORMATION: u16 = 0x5404;
/// `0x5601` RequestVoiceOverMoveCursor — Accessory, 1 params.
pub const MSG_REQUEST_VOICE_OVER_MOVE_CURSOR: u16 = 0x5601;
/// `0x5602` RequestVoiceOverActivateCursor — Accessory, 0 params.
pub const MSG_REQUEST_VOICE_OVER_ACTIVATE_CURSOR: u16 = 0x5602;
/// `0x5603` RequestVoiceOverScrollPage — Accessory, 1 params.
pub const MSG_REQUEST_VOICE_OVER_SCROLL_PAGE: u16 = 0x5603;
/// `0x5606` RequestVoiceOverSpeakText — Accessory, 1 params.
pub const MSG_REQUEST_VOICE_OVER_SPEAK_TEXT: u16 = 0x5606;
/// `0x5608` RequestVoiceOverPauseText — Accessory, 0 params.
pub const MSG_REQUEST_VOICE_OVER_PAUSE_TEXT: u16 = 0x5608;
/// `0x5609` RequestVoiceOverResumeText — Accessory, 0 params.
pub const MSG_REQUEST_VOICE_OVER_RESUME_TEXT: u16 = 0x5609;
/// `0x560B` StartVoiceOverUpdates — Accessory, 3 params.
pub const MSG_START_VOICE_OVER_UPDATES: u16 = 0x560B;
/// `0x560C` VoiceOverUpdate — Device, 3 params.
pub const MSG_VOICE_OVER_UPDATE: u16 = 0x560C;
/// `0x560D` StopVoiceOverUpdates — Accessory, 0 params.
pub const MSG_STOP_VOICE_OVER_UPDATES: u16 = 0x560D;
/// `0x560E` RequestVoiceOverConfiguration — Accessory, 2 params.
pub const MSG_REQUEST_VOICE_OVER_CONFIGURATION: u16 = 0x560E;
/// `0x560F` StartVoiceOverCursorUpdates — Accessory, 4 params.
pub const MSG_START_VOICE_OVER_CURSOR_UPDATES: u16 = 0x560F;
/// `0x5610` VoiceOverCursorUpdate — Device, 4 params.
pub const MSG_VOICE_OVER_CURSOR_UPDATE: u16 = 0x5610;
/// `0x5611` StopVoiceOverCursorUpdates — Accessory, 0 params.
pub const MSG_STOP_VOICE_OVER_CURSOR_UPDATES: u16 = 0x5611;
/// `0x5612` StartVoiceOver — Accessory, 0 params.
pub const MSG_START_VOICE_OVER: u16 = 0x5612;
/// `0x5613` StopVoiceOver — Accessory, 0 params.
pub const MSG_STOP_VOICE_OVER: u16 = 0x5613;
/// `0x5700` RequestWiFiInformation — Accessory, 0 params.
pub const MSG_REQUEST_WI_FI_INFORMATION: u16 = 0x5700;
/// `0x5701` WiFiInformation — Device, 4 params.
pub const MSG_WI_FI_INFORMATION: u16 = 0x5701;
/// `0x5702` RequestAccessoryWiFiConfigurationInformation — Device, 0 params.
pub const MSG_REQUEST_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION: u16 = 0x5702;
/// `0x5703` AccessoryWiFiConfigurationInformation — Accessory, 4 params.
pub const MSG_ACCESSORY_WI_FI_CONFIGURATION_INFORMATION: u16 = 0x5703;
/// `0x6800` StartHID — Accessory, 5 params.
pub const MSG_START_HID: u16 = 0x6800;
/// `0x6801` DeviceHIDReport — Device, 2 params.
pub const MSG_DEVICE_HID_REPORT: u16 = 0x6801;
/// `0x6802` AccessoryHIDReport — Accessory, 2 params.
pub const MSG_ACCESSORY_HID_REPORT: u16 = 0x6802;
/// `0x6803` StopHID — Accessory, 1 params.
pub const MSG_STOP_HID: u16 = 0x6803;
/// `0x6806` StartNativeHID — Device, 0 params.
pub const MSG_START_NATIVE_HID: u16 = 0x6806;
/// `0x6807` HIDComponentUpdate — Device, 2 params.
pub const MSG_HID_COMPONENT_UPDATE: u16 = 0x6807;
/// `0x7E00` StartUSBHostMode — Accessory, 0 params.
pub const MSG_START_USB_HOST_MODE: u16 = 0x7E00;
/// `0x7E01` USBModeNotification — Device, 2 params.
pub const MSG_USB_MODE_NOTIFICATION: u16 = 0x7E01;
/// `0x7E02` StopUSBHostMode — Accessory, 0 params.
pub const MSG_STOP_USB_HOST_MODE: u16 = 0x7E02;
/// `0xA100` StartVehicleStatusUpdates — Device, 16 params.
pub const MSG_START_VEHICLE_STATUS_UPDATES: u16 = 0xA100;
/// `0xA101` VehicleStatusUpdate — Accessory, 25 params.
pub const MSG_VEHICLE_STATUS_UPDATE: u16 = 0xA101;
/// `0xA102` StopVehicleStatusUpdates — Device, 0 params.
pub const MSG_STOP_VEHICLE_STATUS_UPDATES: u16 = 0xA102;
/// `0xAA00` RequestAuthenticationCertificate — Device, 1 params.
pub const MSG_REQUEST_AUTHENTICATION_CERTIFICATE: u16 = 0xAA00;
/// `0xAA01` AuthenticationCertificate — Accessory, 1 params.
pub const MSG_AUTHENTICATION_CERTIFICATE: u16 = 0xAA01;
/// `0xAA02` RequestAuthenticationChallengeResponse — Device, 1 params.
pub const MSG_REQUEST_AUTHENTICATION_CHALLENGE_RESPONSE: u16 = 0xAA02;
/// `0xAA03` AuthenticationResponse — Accessory, 1 params.
pub const MSG_AUTHENTICATION_RESPONSE: u16 = 0xAA03;
/// `0xAA04` AuthenticationFailed — Device, 0 params.
pub const MSG_AUTHENTICATION_FAILED: u16 = 0xAA04;
/// `0xAA05` AuthenticationSucceeded — Device, 0 params.
pub const MSG_AUTHENTICATION_SUCCEEDED: u16 = 0xAA05;
/// `0xAA06` AccessoryAuthenticationSerialNumber — Accessory, 1 params.
pub const MSG_ACCESSORY_AUTHENTICATION_SERIAL_NUMBER: u16 = 0xAA06;
/// `0xAD00` StartAppDiscoveryUpdates — Accessory, 6 params.
pub const MSG_START_APP_DISCOVERY_UPDATES: u16 = 0xAD00;
/// `0xAD01` AppDiscoveryUpdate — Device, 6 params.
pub const MSG_APP_DISCOVERY_UPDATE: u16 = 0xAD01;
/// `0xAD02` StopAppDiscoveryUpdates — Accessory, 0 params.
pub const MSG_STOP_APP_DISCOVERY_UPDATES: u16 = 0xAD02;
/// `0xAD03` RequestAppDiscoveryAppIcons — Accessory, 2 params.
pub const MSG_REQUEST_APP_DISCOVERY_APP_ICONS: u16 = 0xAD03;
/// `0xAD04` AppDiscoveryAppIcon — Device, 4 params.
pub const MSG_APP_DISCOVERY_APP_ICON: u16 = 0xAD04;
/// `0xAE00` StartPowerUpdates — Accessory, 12 params.
pub const MSG_START_POWER_UPDATES: u16 = 0xAE00;
/// `0xAE01` PowerUpdate — Device, 13 params.
pub const MSG_POWER_UPDATE: u16 = 0xAE01;
/// `0xAE02` StopPowerUpdates — Accessory, 0 params.
pub const MSG_STOP_POWER_UPDATES: u16 = 0xAE02;
/// `0xAE03` PowerSourceUpdate — Accessory, 5 params.
pub const MSG_POWER_SOURCE_UPDATE: u16 = 0xAE03;
/// `0xAE05` AccessoryPowerUpdate — Accessory, 1 params.
pub const MSG_ACCESSORY_POWER_UPDATE: u16 = 0xAE05;
/// `0xB100` StartBluetoothLowEnergyUpdates — Device, 3 params.
pub const MSG_START_BLUETOOTH_LOW_ENERGY_UPDATES: u16 = 0xB100;
/// `0xB101` DeviceBluetoothLowEnergyUpdate — Device, 3 params.
pub const MSG_DEVICE_BLUETOOTH_LOW_ENERGY_UPDATE: u16 = 0xB101;
/// `0xB102` AccessoryBluetoothLowEnergyUpdate — Accessory, 3 params.
pub const MSG_ACCESSORY_BLUETOOTH_LOW_ENERGY_UPDATE: u16 = 0xB102;
/// `0xB103` AccessoryBluetoothLowEnergyPairingInformation — Accessory, 2 params.
pub const MSG_ACCESSORY_BLUETOOTH_LOW_ENERGY_PAIRING_INFORMATION: u16 = 0xB103;
/// `0xB104` DeviceBluetoothLowEnergyPairingData — Device, 2 params.
pub const MSG_DEVICE_BLUETOOTH_LOW_ENERGY_PAIRING_DATA: u16 = 0xB104;
/// `0xB105` AccessoryBluetoothLowEnergyPairingData — Accessory, 2 params.
pub const MSG_ACCESSORY_BLUETOOTH_LOW_ENERGY_PAIRING_DATA: u16 = 0xB105;
/// `0xB106` DeviceBluetoothLowEnergyPairingInformation — Device, 2 params.
pub const MSG_DEVICE_BLUETOOTH_LOW_ENERGY_PAIRING_INFORMATION: u16 = 0xB106;
/// `0xB107` StopBluetoothLowEnergyUpdates — Device, 0 params.
pub const MSG_STOP_BLUETOOTH_LOW_ENERGY_UPDATES: u16 = 0xB107;
/// `0xDA00` StartUSBDeviceModeAudio — Accessory, 0 params.
pub const MSG_START_USB_DEVICE_MODE_AUDIO: u16 = 0xDA00;
/// `0xDA01` USBDeviceModeAudioInformation — Device, 3 params.
pub const MSG_USB_DEVICE_MODE_AUDIO_INFORMATION: u16 = 0xDA01;
/// `0xDA02` StopUSBDeviceModeAudio — Accessory, 0 params.
pub const MSG_STOP_USB_DEVICE_MODE_AUDIO: u16 = 0xDA02;
/// `0xEA00` StartExternalAccessoryProtocolSession — Device, 2 params.
pub const MSG_START_EXTERNAL_ACCESSORY_PROTOCOL_SESSION: u16 = 0xEA00;
/// `0xEA01` StopExternalAccessoryProtocolSession — Device, 1 params.
pub const MSG_STOP_EXTERNAL_ACCESSORY_PROTOCOL_SESSION: u16 = 0xEA01;
/// `0xEA02` RequestAppLaunch — Accessory, 2 params.
pub const MSG_REQUEST_APP_LAUNCH: u16 = 0xEA02;
/// `0xEA03` StatusExternalAccessoryProtocolSession — Accessory, 2 params.
pub const MSG_STATUS_EXTERNAL_ACCESSORY_PROTOCOL_SESSION: u16 = 0xEA03;
/// `0xFFF0` GPRMCDataStatusValuesNotification — Device, 3 params.
pub const MSG_GPRMC_DATA_STATUS_VALUES_NOTIFICATION: u16 = 0xFFF0;
/// `0xFFFA` StartLocationInformation — Device, 15 params.
pub const MSG_START_LOCATION_INFORMATION: u16 = 0xFFFA;
/// `0xFFFB` LocationInformation — Accessory, 1 params.
pub const MSG_LOCATION_INFORMATION: u16 = 0xFFFB;
/// `0xFFFC` StopLocationInformation — Device, 0 params.
pub const MSG_STOP_LOCATION_INFORMATION: u16 = 0xFFFC;

// ===== MFi4 authentication messages (126 messages) =====
/// `0x5100` RequestAuthSetup — Mfi(81), 5 params.
pub const MFI_REQUEST_AUTH_SETUP: u16 = 0x5100;
/// `0x5102` AuthSetupFailed — Mfi(81), 0 params.
pub const MFI_AUTH_SETUP_FAILED: u16 = 0x5102;
/// `0x5110` RequestAuthStart — Mfi(81), 2 params.
pub const MFI_REQUEST_AUTH_START: u16 = 0x5110;
/// `0x5111` AuthStart — Mfi(81), 3 params.
pub const MFI_AUTH_START: u16 = 0x5111;
/// `0x5112` RequestAuthCert — Mfi(81), 2 params.
pub const MFI_REQUEST_AUTH_CERT: u16 = 0x5112;
/// `0x5113` AuthCert — Mfi(81), 3 params.
pub const MFI_AUTH_CERT: u16 = 0x5113;
/// `0x5114` AuthFinish — Mfi(81), 2 params.
pub const MFI_AUTH_FINISH: u16 = 0x5114;
/// `0x5115` RequestAuthChallengeResponse — Mfi(81), 1 params.
pub const MFI_REQUEST_AUTH_CHALLENGE_RESPONSE: u16 = 0x5115;
/// `0x5120` RequestTemporaryCert — Mfi(81), 4 params.
pub const MFI_REQUEST_TEMPORARY_CERT: u16 = 0x5120;
/// `0x5122` RequestSignWithTemporaryKey — Mfi(81), 3 params.
pub const MFI_REQUEST_SIGN_WITH_TEMPORARY_KEY: u16 = 0x5122;
/// `0x5130` RequestUserNVMRead — Mfi(81), 4 params.
pub const MFI_REQUEST_USER_NVM_READ: u16 = 0x5130;
/// `0x5131` RequestVendorNVMRead — Mfi(81), 4 params.
pub const MFI_REQUEST_VENDOR_NVM_READ: u16 = 0x5131;
/// `0x5133` RequestUserNVMWrite — Mfi(81), 1 params.
pub const MFI_REQUEST_USER_NVM_WRITE: u16 = 0x5133;
/// `0x5134` RequestVendorNVMWrite — Mfi(81), 1 params.
pub const MFI_REQUEST_VENDOR_NVM_WRITE: u16 = 0x5134;
/// `0x5136` RequestUserNVMErase — Mfi(81), 1 params.
pub const MFI_REQUEST_USER_NVM_ERASE: u16 = 0x5136;
/// `0x5137` RequestVendorNVMErase — Mfi(81), 1 params.
pub const MFI_REQUEST_VENDOR_NVM_ERASE: u16 = 0x5137;
/// `0x5139` RequestNVMPublicKeyChallenge — Mfi(81), 1 params.
pub const MFI_REQUEST_NVM_PUBLIC_KEY_CHALLENGE: u16 = 0x5139;
/// `0x513B` RequestNVMWritePublicKey — Mfi(81), 5 params.
pub const MFI_REQUEST_NVM_WRITE_PUBLIC_KEY: u16 = 0x513B;
/// `0x513C` RequestNVMErasePublicKey — Mfi(81), 4 params.
pub const MFI_REQUEST_NVM_ERASE_PUBLIC_KEY: u16 = 0x513C;
/// `0x513D` RequestNVMAuthStart — Mfi(81), 1 params.
pub const MFI_REQUEST_NVM_AUTH_START: u16 = 0x513D;
/// `0x513F` RequestNVMAuthFinish — Mfi(81), 3 params.
pub const MFI_REQUEST_NVM_AUTH_FINISH: u16 = 0x513F;
/// `0x5142` RequestManufacturerNVMRead — Mfi(81), 4 params.
pub const MFI_REQUEST_MANUFACTURER_NVM_READ: u16 = 0x5142;
/// `0x5143` RequestManufacturerNVMWrite — Mfi(81), 1 params.
pub const MFI_REQUEST_MANUFACTURER_NVM_WRITE: u16 = 0x5143;
/// `0x5144` RequestManufacturerNVMErase — Mfi(81), 1 params.
pub const MFI_REQUEST_MANUFACTURER_NVM_ERASE: u16 = 0x5144;
/// `0x5147` RequestNVMAuthReset — Mfi(81), 1 params.
pub const MFI_REQUEST_NVM_AUTH_RESET: u16 = 0x5147;
/// `0x5149` RequestNVMOperation — Mfi(81), 1 params.
pub const MFI_REQUEST_NVM_OPERATION: u16 = 0x5149;
/// `0x5150` RequestProvisioningStatus — Mfi(81), 0 params.
pub const MFI_REQUEST_PROVISIONING_STATUS: u16 = 0x5150;
/// `0x5152` RequestProvisionCertChain — Mfi(81), 6 params.
pub const MFI_REQUEST_PROVISION_CERT_CHAIN: u16 = 0x5152;
/// `0x5163` RequestiAP2RelayRead — Mfi(81), 0 params.
pub const MFI_REQUESTI_AP2_RELAY_READ: u16 = 0x5163;
/// `0x5164` iAP2RelayRemote — Mfi(81), 1 params.
pub const MFI_I_AP2_RELAY_REMOTE: u16 = 0x5164;
/// `0x5165` iAP2RelayFailed — Mfi(81), 0 params.
pub const MFI_I_AP2_RELAY_FAILED: u16 = 0x5165;
/// `0x5166` iAP2RelaySucceeded — Mfi(81), 0 params.
pub const MFI_I_AP2_RELAY_SUCCEEDED: u16 = 0x5166;
/// `0x516A` RequestEAPRelayRead — Mfi(81), 0 params.
pub const MFI_REQUEST_EAP_RELAY_READ: u16 = 0x516A;
/// `0x516B` EAPRelayRemote — Mfi(81), 1 params.
pub const MFI_EAP_RELAY_REMOTE: u16 = 0x516B;
/// `0x516C` EAPRelayFailed — Mfi(81), 0 params.
pub const MFI_EAP_RELAY_FAILED: u16 = 0x516C;
/// `0x516D` EAPRelaySucceeded — Mfi(81), 0 params.
pub const MFI_EAP_RELAY_SUCCEEDED: u16 = 0x516D;
/// `0x5170` RequestGPIORemoteOutputA — Mfi(81), 1 params.
pub const MFI_REQUEST_GPIO_REMOTE_OUTPUT_A: u16 = 0x5170;
/// `0x5171` RequestGPIORemoteOutputB — Mfi(81), 1 params.
pub const MFI_REQUEST_GPIO_REMOTE_OUTPUT_B: u16 = 0x5171;
/// `0x5172` RequestGPIORemoteInputA — Mfi(81), 0 params.
pub const MFI_REQUEST_GPIO_REMOTE_INPUT_A: u16 = 0x5172;
/// `0x5174` RequestGPIORemoteInputB — Mfi(81), 0 params.
pub const MFI_REQUEST_GPIO_REMOTE_INPUT_B: u16 = 0x5174;
/// `0x5176` RequestGPIODownstreamPowerSupply — Mfi(81), 0 params.
pub const MFI_REQUEST_GPIO_DOWNSTREAM_POWER_SUPPLY: u16 = 0x5176;
/// `0x51FD` AuthenticationReset — Mfi(81), 0 params.
pub const MFI_AUTHENTICATION_RESET: u16 = 0x51FD;
/// `0x51FE` AuthenticationFailed — Mfi(81), 0 params.
pub const MFI_AUTHENTICATION_FAILED: u16 = 0x51FE;
/// `0x51FF` AuthenticationSucceeded — Mfi(81), 0 params.
pub const MFI_AUTHENTICATION_SUCCEEDED: u16 = 0x51FF;
/// `0x5201` AuthSetup — Mfi(82), 6 params.
pub const MFI_AUTH_SETUP: u16 = 0x5201;
/// `0x5202` AuthSetupFailed — Mfi(82), 0 params.
pub const MFI_AUTH_SETUP_FAILED_5202: u16 = 0x5202;
/// `0x5211` AuthStart — Mfi(82), 3 params.
pub const MFI_AUTH_START_5211: u16 = 0x5211;
/// `0x5212` RequestAuthCert — Mfi(82), 2 params.
pub const MFI_REQUEST_AUTH_CERT_5212: u16 = 0x5212;
/// `0x5213` AuthCert — Mfi(82), 3 params.
pub const MFI_AUTH_CERT_5213: u16 = 0x5213;
/// `0x5214` AuthFinish — Mfi(82), 2 params.
pub const MFI_AUTH_FINISH_5214: u16 = 0x5214;
/// `0x5216` AuthChallengeResponse — Mfi(82), 3 params.
pub const MFI_AUTH_CHALLENGE_RESPONSE: u16 = 0x5216;
/// `0x5221` AttestationResponse — Mfi(82), 7 params.
pub const MFI_ATTESTATION_RESPONSE: u16 = 0x5221;
/// `0x5223` SigningResponse — Mfi(82), 2 params.
pub const MFI_SIGNING_RESPONSE: u16 = 0x5223;
/// `0x5232` NVMReadResponse — Mfi(82), 7 params.
pub const MFI_NVM_READ_RESPONSE: u16 = 0x5232;
/// `0x5235` NVMWriteResponse — Mfi(82), 2 params.
pub const MFI_NVM_WRITE_RESPONSE: u16 = 0x5235;
/// `0x5238` NVMEraseResponse — Mfi(82), 2 params.
pub const MFI_NVM_ERASE_RESPONSE: u16 = 0x5238;
/// `0x523A` NVMPublicKeyChallenge — Mfi(82), 3 params.
pub const MFI_NVM_PUBLIC_KEY_CHALLENGE: u16 = 0x523A;
/// `0x523E` NVMAuthStart — Mfi(82), 3 params.
pub const MFI_NVM_AUTH_START: u16 = 0x523E;
/// `0x5240` NVMAuthFailed — Mfi(82), 0 params.
pub const MFI_NVM_AUTH_FAILED: u16 = 0x5240;
/// `0x5241` NVMAuthSucceeded — Mfi(82), 0 params.
pub const MFI_NVM_AUTH_SUCCEEDED: u16 = 0x5241;
/// `0x5245` NVMAuthFinish — Mfi(82), 3 params.
pub const MFI_NVM_AUTH_FINISH: u16 = 0x5245;
/// `0x5248` NVMAuthReset — Mfi(82), 1 params.
pub const MFI_NVM_AUTH_RESET: u16 = 0x5248;
/// `0x524A` NVMOperationResponse — Mfi(82), 1 params.
pub const MFI_NVM_OPERATION_RESPONSE: u16 = 0x524A;
/// `0x5251` ProvisioningStatus — Mfi(82), 1 params.
pub const MFI_PROVISIONING_STATUS: u16 = 0x5251;
/// `0x5253` ProvisioningResponse — Mfi(82), 6 params.
pub const MFI_PROVISIONING_RESPONSE: u16 = 0x5253;
/// `0x5264` iAP2RelayRemote — Mfi(82), 1 params.
pub const MFI_I_AP2_RELAY_REMOTE_5264: u16 = 0x5264;
/// `0x5265` iAP2RelayFailed — Mfi(82), 0 params.
pub const MFI_I_AP2_RELAY_FAILED_5265: u16 = 0x5265;
/// `0x5266` iAP2RelaySucceeded — Mfi(82), 0 params.
pub const MFI_I_AP2_RELAY_SUCCEEDED_5266: u16 = 0x5266;
/// `0x526B` EAPRelayRemote — Mfi(82), 1 params.
pub const MFI_EAP_RELAY_REMOTE_526B: u16 = 0x526B;
/// `0x526C` EAPRelayFailed — Mfi(82), 0 params.
pub const MFI_EAP_RELAY_FAILED_526C: u16 = 0x526C;
/// `0x526D` EAPRelaySucceeded — Mfi(82), 0 params.
pub const MFI_EAP_RELAY_SUCCEEDED_526D: u16 = 0x526D;
/// `0x5273` GPIORemoteInputA — Mfi(82), 1 params.
pub const MFI_GPIO_REMOTE_INPUT_A: u16 = 0x5273;
/// `0x5275` GPIORemoteInputB — Mfi(82), 1 params.
pub const MFI_GPIO_REMOTE_INPUT_B: u16 = 0x5275;
/// `0x5277` GPIORemoteOutputA — Mfi(82), 1 params.
pub const MFI_GPIO_REMOTE_OUTPUT_A: u16 = 0x5277;
/// `0x5278` GPIORemoteOutputB — Mfi(82), 1 params.
pub const MFI_GPIO_REMOTE_OUTPUT_B: u16 = 0x5278;
/// `0x52FB` SigningFailed — Mfi(82), 0 params.
pub const MFI_SIGNING_FAILED: u16 = 0x52FB;
/// `0x52FC` AttestationFailed — Mfi(82), 0 params.
pub const MFI_ATTESTATION_FAILED: u16 = 0x52FC;
/// `0x52FD` AuthenticationReset — Mfi(82), 0 params.
pub const MFI_AUTHENTICATION_RESET_52FD: u16 = 0x52FD;
/// `0x52FE` AuthenticationFailed — Mfi(82), 0 params.
pub const MFI_AUTHENTICATION_FAILED_52FE: u16 = 0x52FE;
/// `0x52FF` AuthenticationSucceeded — Mfi(82), 0 params.
pub const MFI_AUTHENTICATION_SUCCEEDED_52FF: u16 = 0x52FF;
/// `0x5320` RequestTemporaryCert — Mfi(83), 4 params.
pub const MFI_REQUEST_TEMPORARY_CERT_5320: u16 = 0x5320;
/// `0x5322` RequestSignWithTemporaryKey — Mfi(83), 3 params.
pub const MFI_REQUEST_SIGN_WITH_TEMPORARY_KEY_5322: u16 = 0x5322;
/// `0x5324` RequestVerifySignature — Mfi(83), 3 params.
pub const MFI_REQUEST_VERIFY_SIGNATURE: u16 = 0x5324;
/// `0x5326` RequestVerifyCertChain — Mfi(83), 2 params.
pub const MFI_REQUEST_VERIFY_CERT_CHAIN: u16 = 0x5326;
/// `0x5328` RequestMFi3AuthChallenge — Mfi(83), 0 params.
pub const MFI_REQUEST_M_FI3_AUTH_CHALLENGE: u16 = 0x5328;
/// `0x532A` RequestMFi3AuthVerify — Mfi(83), 2 params.
pub const MFI_REQUEST_M_FI3_AUTH_VERIFY: u16 = 0x532A;
/// `0x532B` RequestWPCAuthSignature — Mfi(83), 1 params.
pub const MFI_REQUEST_WPC_AUTH_SIGNATURE: u16 = 0x532B;
/// `0x532C` RequestMatterAuthSignature — Mfi(83), 1 params.
pub const MFI_REQUEST_MATTER_AUTH_SIGNATURE: u16 = 0x532C;
/// `0x5330` RequestUserNVMRead — Mfi(83), 4 params.
pub const MFI_REQUEST_USER_NVM_READ_5330: u16 = 0x5330;
/// `0x5331` RequestVendorNVMRead — Mfi(83), 4 params.
pub const MFI_REQUEST_VENDOR_NVM_READ_5331: u16 = 0x5331;
/// `0x5334` RequestVendorNVMWrite — Mfi(83), 1 params.
pub const MFI_REQUEST_VENDOR_NVM_WRITE_5334: u16 = 0x5334;
/// `0x5337` RequestVendorNVMErase — Mfi(83), 1 params.
pub const MFI_REQUEST_VENDOR_NVM_ERASE_5337: u16 = 0x5337;
/// `0x5339` RequestNVMPublicKeyChallenge — Mfi(83), 1 params.
pub const MFI_REQUEST_NVM_PUBLIC_KEY_CHALLENGE_5339: u16 = 0x5339;
/// `0x533B` RequestNVMWritePublicKey — Mfi(83), 5 params.
pub const MFI_REQUEST_NVM_WRITE_PUBLIC_KEY_533B: u16 = 0x533B;
/// `0x533C` RequestNVMErasePublicKey — Mfi(83), 4 params.
pub const MFI_REQUEST_NVM_ERASE_PUBLIC_KEY_533C: u16 = 0x533C;
/// `0x5342` RequestManufacturerNVMRead — Mfi(83), 4 params.
pub const MFI_REQUEST_MANUFACTURER_NVM_READ_5342: u16 = 0x5342;
/// `0x5343` RequestManufacturerNVMWrite — Mfi(83), 1 params.
pub const MFI_REQUEST_MANUFACTURER_NVM_WRITE_5343: u16 = 0x5343;
/// `0x5344` RequestManufacturerNVMErase — Mfi(83), 1 params.
pub const MFI_REQUEST_MANUFACTURER_NVM_ERASE_5344: u16 = 0x5344;
/// `0x5345` RequestNVMOverlayWrite — Mfi(83), 1 params.
pub const MFI_REQUEST_NVM_OVERLAY_WRITE: u16 = 0x5345;
/// `0x5346` RequestNVMOverlayErase — Mfi(83), 1 params.
pub const MFI_REQUEST_NVM_OVERLAY_ERASE: u16 = 0x5346;
/// `0x5360` iAP2RelayI2CtoNFC — Mfi(83), 1 params.
pub const MFI_I_AP2_RELAY_I2_CTO_NFC: u16 = 0x5360;
/// `0x5361` iAP2RelayI2CtoI2C — Mfi(83), 1 params.
pub const MFI_I_AP2_RELAY_I2_CTO_I2_C: u16 = 0x5361;
/// `0x5367` EAPRelayI2CtoNFC — Mfi(83), 1 params.
pub const MFI_EAP_RELAY_I2_CTO_NFC: u16 = 0x5367;
/// `0x5368` EAPRelayI2CtoI2C — Mfi(83), 1 params.
pub const MFI_EAP_RELAY_I2_CTO_I2_C: u16 = 0x5368;
/// `0x5380` RequestConfigWrite — Mfi(82), 2 params.
pub const MFI_REQUEST_CONFIG_WRITE: u16 = 0x5380;
/// `0x5400` RequestAuthSetup — Mfi(82), 5 params.
pub const MFI_REQUEST_AUTH_SETUP_5400: u16 = 0x5400;
/// `0x5421` AttestationResponse — Mfi(84), 7 params.
pub const MFI_ATTESTATION_RESPONSE_5421: u16 = 0x5421;
/// `0x5423` SigningResponse — Mfi(84), 2 params.
pub const MFI_SIGNING_RESPONSE_5423: u16 = 0x5423;
/// `0x5425` VerifySignatureResponse — Mfi(84), 1 params.
pub const MFI_VERIFY_SIGNATURE_RESPONSE: u16 = 0x5425;
/// `0x5427` VerifyCertChainResponse — Mfi(84), 1 params.
pub const MFI_VERIFY_CERT_CHAIN_RESPONSE: u16 = 0x5427;
/// `0x5429` MFi3AuthChallengeResponse — Mfi(84), 1 params.
pub const MFI_M_FI3_AUTH_CHALLENGE_RESPONSE: u16 = 0x5429;
/// `0x5432` NVMReadResponse — Mfi(84), 7 params.
pub const MFI_NVM_READ_RESPONSE_5432: u16 = 0x5432;
/// `0x5435` NVMWriteResponse — Mfi(84), 2 params.
pub const MFI_NVM_WRITE_RESPONSE_5435: u16 = 0x5435;
/// `0x5438` NVMEraseResponse — Mfi(84), 2 params.
pub const MFI_NVM_ERASE_RESPONSE_5438: u16 = 0x5438;
/// `0x543A` NVMPublicKeyChallenge — Mfi(84), 3 params.
pub const MFI_NVM_PUBLIC_KEY_CHALLENGE_543A: u16 = 0x543A;
/// `0x5462` iAP2RelayI2C — Mfi(84), 1 params.
pub const MFI_I_AP2_RELAY_I2_C: u16 = 0x5462;
/// `0x5465` iAP2RelayFailed — Mfi(84), 0 params.
pub const MFI_I_AP2_RELAY_FAILED_5465: u16 = 0x5465;
/// `0x5466` iAP2RelaySucceeded — Mfi(84), 0 params.
pub const MFI_I_AP2_RELAY_SUCCEEDED_5466: u16 = 0x5466;
/// `0x5469` EAPRelayI2C — Mfi(84), 1 params.
pub const MFI_EAP_RELAY_I2_C: u16 = 0x5469;
/// `0x546C` EAPRelayFailed — Mfi(84), 0 params.
pub const MFI_EAP_RELAY_FAILED_546C: u16 = 0x546C;
/// `0x546D` EAPRelaySucceeded — Mfi(84), 0 params.
pub const MFI_EAP_RELAY_SUCCEEDED_546D: u16 = 0x546D;
/// `0x5481` ConfigResponse — Mfi(82), 2 params.
pub const MFI_CONFIG_RESPONSE: u16 = 0x5481;
/// `0x54FB` SigningFailed — Mfi(84), 0 params.
pub const MFI_SIGNING_FAILED_54FB: u16 = 0x54FB;
/// `0x54FC` AttestationFailed — Mfi(84), 0 params.
pub const MFI_ATTESTATION_FAILED_54FC: u16 = 0x54FC;
/// `0x54FE` AuthenticationFailed — Mfi(84), 0 params.
pub const MFI_AUTHENTICATION_FAILED_54FE: u16 = 0x54FE;
/// `0x54FF` AuthenticationSucceeded — Mfi(84), 0 params.
pub const MFI_AUTHENTICATION_SUCCEEDED_54FF: u16 = 0x54FF;

/// Authoritative (message-id, name, originator) table for the iAP2 control plane. Use this to
/// validate that every id declared in IdentificationInformation param 6 is `Source::Accessory`
/// and every id in param 7 is `Source::Device`.
pub const IAP2_MESSAGES: &[(u16, &str, Source)] = &[
    (0x00B0, "StartOOBBTPairing", Source::Device),
    (0x00B1, "OOBBTPairingAccessoryInformation", Source::Accessory),
    (0x00B2, "OOBBTPairingLinkKeyInformation", Source::Device),
    (0x00B3, "OOBBTPairingCompletionInformation", Source::Accessory),
    (0x00B4, "StopOOBBTPairing", Source::Device),
    (0x0B00, "StartBluetoothPairing", Source::Device),
    (0x0B01, "BluetoothPairingAccessoryInformation", Source::Accessory),
    (0x0B02, "BluetoothPairingStatus", Source::Accessory),
    (0x0B03, "StopBluetoothPairing", Source::Device),
    (0x0D00, "StartRoadObjectDetectionUpdates", Source::Device),
    (0x0D01, "RoadObjectDetectionUpdate", Source::Accessory),
    (0x0D02, "StopRoadObjectDetectionUpdates", Source::Device),
    (0x10DA, "StartDestinationInformation", Source::Accessory),
    (0x10DB, "DestinationInformation", Source::Device),
    (0x10DC, "StopDestinationInformation", Source::Accessory),
    (0x10DD, "DestinationInformationStatus", Source::Accessory),
    (0x1D00, "StartIdentification", Source::Device),
    (0x1D01, "IdentificationInformation", Source::Accessory),
    (0x1D02, "IdentificationAccepted", Source::Device),
    (0x1D03, "IdentificationRejected", Source::Device),
    (0x1D05, "CancelIdentification", Source::Accessory),
    (0x1D06, "IdentificationInformationUpdate", Source::Accessory),
    (0x4154, "StartCallStateUpdates", Source::Accessory),
    (0x4155, "CallStateUpdate", Source::Device),
    (0x4156, "StopCallStateUpdates", Source::Accessory),
    (0x4157, "StartCommunicationsUpdates", Source::Accessory),
    (0x4158, "CommunicationsUpdate", Source::Device),
    (0x4159, "StopCommunicationsUpdates", Source::Accessory),
    (0x415A, "InitiateCall", Source::Accessory),
    (0x415B, "AcceptCall", Source::Accessory),
    (0x415C, "EndCall", Source::Accessory),
    (0x415D, "SwapCalls", Source::Accessory),
    (0x415E, "MergeCalls", Source::Accessory),
    (0x415F, "HoldStatusUpdate", Source::Accessory),
    (0x4160, "MuteStatusUpdate", Source::Accessory),
    (0x4161, "SendDTMF", Source::Accessory),
    (0x4170, "StartListUpdates", Source::Accessory),
    (0x4171, "ListUpdate", Source::Device),
    (0x4172, "StopListUpdates", Source::Accessory),
    (0x4300, "CarPlayAvailability", Source::Device),
    (0x4301, "CarPlayStartSession", Source::Accessory),
    (0x4302, "AvailableDigitalCarKeys", Source::Accessory),
    (0x4303, "MatchedDigitalCarKeys", Source::Device),
    (0x4C00, "StartMediaLibraryInformation", Source::Accessory),
    (0x4C01, "MediaLibraryInformation", Source::Device),
    (0x4C02, "StopMediaLibraryInformation", Source::Accessory),
    (0x4C03, "StartMediaLibraryUpdates", Source::Accessory),
    (0x4C04, "MediaLibraryUpdate", Source::Device),
    (0x4C05, "StopMediaLibraryUpdates", Source::Accessory),
    (0x4C06, "PlayMediaLibraryCurrentSelection", Source::Accessory),
    (0x4C07, "PlayMediaLibraryItems", Source::Accessory),
    (0x4C08, "PlayMediaLibraryCollection", Source::Accessory),
    (0x4C09, "PlayMediaLibrarySpecial", Source::Accessory),
    (0x4E01, "BluetoothComponentInformation", Source::Accessory),
    (0x4E03, "StartBluetoothConnectionUpdates", Source::Accessory),
    (0x4E04, "BluetoothConnectionUpdate", Source::Device),
    (0x4E05, "StopBluetoothConnectionUpdates", Source::Accessory),
    (0x4E09, "DeviceInformationUpdate", Source::Device),
    (0x4E0A, "DeviceLanguageUpdate", Source::Device),
    (0x4E0B, "DeviceTimeUpdate", Source::Device),
    (0x4E0C, "DeviceUUIDUpdate", Source::Device),
    (0x4E0D, "WirelessCarPlayUpdate", Source::Device),
    (0x4E0E, "DeviceTransportIdentifierNotification", Source::Device),
    (0x5000, "StartNowPlayingUpdates", Source::Accessory),
    (0x5001, "NowPlayingUpdate", Source::Device),
    (0x5002, "StopNowPlayingUpdates", Source::Accessory),
    (0x5003, "SetNowPlayingInformation", Source::Accessory),
    (0x5200, "StartRouteGuidanceUpdates", Source::Accessory),
    (0x5201, "RouteGuidanceUpdate", Source::Device),
    (0x5202, "RouteGuidanceManeuverInformation", Source::Device),
    (0x5203, "StopRouteGuidanceUpdates", Source::Accessory),
    (0x5204, "LaneGuidanceInformation", Source::Device),
    (0x5400, "StartAssistiveTouch", Source::Accessory),
    (0x5401, "StopAssistiveTouch", Source::Accessory),
    (0x5402, "StartAssistiveTouchInformation", Source::Accessory),
    (0x5403, "AssistiveTouchInformation", Source::Device),
    (0x5404, "StopAssistiveTouchInformation", Source::Accessory),
    (0x5601, "RequestVoiceOverMoveCursor", Source::Accessory),
    (0x5602, "RequestVoiceOverActivateCursor", Source::Accessory),
    (0x5603, "RequestVoiceOverScrollPage", Source::Accessory),
    (0x5606, "RequestVoiceOverSpeakText", Source::Accessory),
    (0x5608, "RequestVoiceOverPauseText", Source::Accessory),
    (0x5609, "RequestVoiceOverResumeText", Source::Accessory),
    (0x560B, "StartVoiceOverUpdates", Source::Accessory),
    (0x560C, "VoiceOverUpdate", Source::Device),
    (0x560D, "StopVoiceOverUpdates", Source::Accessory),
    (0x560E, "RequestVoiceOverConfiguration", Source::Accessory),
    (0x560F, "StartVoiceOverCursorUpdates", Source::Accessory),
    (0x5610, "VoiceOverCursorUpdate", Source::Device),
    (0x5611, "StopVoiceOverCursorUpdates", Source::Accessory),
    (0x5612, "StartVoiceOver", Source::Accessory),
    (0x5613, "StopVoiceOver", Source::Accessory),
    (0x5700, "RequestWiFiInformation", Source::Accessory),
    (0x5701, "WiFiInformation", Source::Device),
    (0x5702, "RequestAccessoryWiFiConfigurationInformation", Source::Device),
    (0x5703, "AccessoryWiFiConfigurationInformation", Source::Accessory),
    (0x6800, "StartHID", Source::Accessory),
    (0x6801, "DeviceHIDReport", Source::Device),
    (0x6802, "AccessoryHIDReport", Source::Accessory),
    (0x6803, "StopHID", Source::Accessory),
    (0x6806, "StartNativeHID", Source::Device),
    (0x6807, "HIDComponentUpdate", Source::Device),
    (0x7E00, "StartUSBHostMode", Source::Accessory),
    (0x7E01, "USBModeNotification", Source::Device),
    (0x7E02, "StopUSBHostMode", Source::Accessory),
    (0xA100, "StartVehicleStatusUpdates", Source::Device),
    (0xA101, "VehicleStatusUpdate", Source::Accessory),
    (0xA102, "StopVehicleStatusUpdates", Source::Device),
    (0xAA00, "RequestAuthenticationCertificate", Source::Device),
    (0xAA01, "AuthenticationCertificate", Source::Accessory),
    (0xAA02, "RequestAuthenticationChallengeResponse", Source::Device),
    (0xAA03, "AuthenticationResponse", Source::Accessory),
    (0xAA04, "AuthenticationFailed", Source::Device),
    (0xAA05, "AuthenticationSucceeded", Source::Device),
    (0xAA06, "AccessoryAuthenticationSerialNumber", Source::Accessory),
    (0xAD00, "StartAppDiscoveryUpdates", Source::Accessory),
    (0xAD01, "AppDiscoveryUpdate", Source::Device),
    (0xAD02, "StopAppDiscoveryUpdates", Source::Accessory),
    (0xAD03, "RequestAppDiscoveryAppIcons", Source::Accessory),
    (0xAD04, "AppDiscoveryAppIcon", Source::Device),
    (0xAE00, "StartPowerUpdates", Source::Accessory),
    (0xAE01, "PowerUpdate", Source::Device),
    (0xAE02, "StopPowerUpdates", Source::Accessory),
    (0xAE03, "PowerSourceUpdate", Source::Accessory),
    (0xAE05, "AccessoryPowerUpdate", Source::Accessory),
    (0xB100, "StartBluetoothLowEnergyUpdates", Source::Device),
    (0xB101, "DeviceBluetoothLowEnergyUpdate", Source::Device),
    (0xB102, "AccessoryBluetoothLowEnergyUpdate", Source::Accessory),
    (0xB103, "AccessoryBluetoothLowEnergyPairingInformation", Source::Accessory),
    (0xB104, "DeviceBluetoothLowEnergyPairingData", Source::Device),
    (0xB105, "AccessoryBluetoothLowEnergyPairingData", Source::Accessory),
    (0xB106, "DeviceBluetoothLowEnergyPairingInformation", Source::Device),
    (0xB107, "StopBluetoothLowEnergyUpdates", Source::Device),
    (0xDA00, "StartUSBDeviceModeAudio", Source::Accessory),
    (0xDA01, "USBDeviceModeAudioInformation", Source::Device),
    (0xDA02, "StopUSBDeviceModeAudio", Source::Accessory),
    (0xEA00, "StartExternalAccessoryProtocolSession", Source::Device),
    (0xEA01, "StopExternalAccessoryProtocolSession", Source::Device),
    (0xEA02, "RequestAppLaunch", Source::Accessory),
    (0xEA03, "StatusExternalAccessoryProtocolSession", Source::Accessory),
    (0xFFF0, "GPRMCDataStatusValuesNotification", Source::Device),
    (0xFFFA, "StartLocationInformation", Source::Device),
    (0xFFFB, "LocationInformation", Source::Accessory),
    (0xFFFC, "StopLocationInformation", Source::Device),
];

/// Look up who originates a control-plane message id.
pub fn source_of(id: u16) -> Option<Source> {
    IAP2_MESSAGES.iter().find(|(mid, _, _)| *mid == id).map(|(_, _, s)| *s)
}

/// Apple's own name for a control-plane message id.
pub fn name_of(id: u16) -> Option<&'static str> {
    IAP2_MESSAGES.iter().find(|(mid, _, _)| *mid == id).map(|(_, n, _)| *n)
}

/// Parameter ids inside IdentificationInformation (`0x1D01`) — the accessory capability
/// declaration. Names are Apple's own. Group params expand further (see `ident` submodule).
pub mod ident {
    /// Name — `utf8`.
    pub const NAME: u16 = 0;
    /// ModelIdentifier — `utf8`.
    pub const MODEL_IDENTIFIER: u16 = 1;
    /// Manufacturer — `utf8`.
    pub const MANUFACTURER: u16 = 2;
    /// SerialNumber — `utf8`.
    pub const SERIAL_NUMBER: u16 = 3;
    /// FirmwareVersion — `utf8`.
    pub const FIRMWARE_VERSION: u16 = 4;
    /// HardwareVersion — `utf8`.
    pub const HARDWARE_VERSION: u16 = 5;
    /// MessagesSentByAccessory — `uint16[0+]`.
    pub const MESSAGES_SENT_BY_ACCESSORY: u16 = 6;
    /// MessagesReceivedFromDevice — `uint16[0+]`.
    pub const MESSAGES_RECEIVED_FROM_DEVICE: u16 = 7;
    /// PowerProvidingCapability — `enum`.
    pub const POWER_PROVIDING_CAPABILITY: u16 = 8;
    /// MaximumCurrentDrawnFromDevice — `uint16`.
    pub const MAXIMUM_CURRENT_DRAWN_FROM_DEVICE: u16 = 9;
    /// SupportedExternalAccessoryProtocol — `group`.
    pub const SUPPORTED_EXTERNAL_ACCESSORY_PROTOCOL: u16 = 10;
    /// AppMatchTeamID — `utf8`.
    pub const APP_MATCH_TEAM_ID: u16 = 11;
    /// CurrentLanguage — `utf8`.
    pub const CURRENT_LANGUAGE: u16 = 12;
    /// SupportedLanguage — `utf8`.
    pub const SUPPORTED_LANGUAGE: u16 = 13;
    /// UARTTransportComponent — `group`.
    pub const UART_TRANSPORT_COMPONENT: u16 = 14;
    /// USBDeviceTransportComponent — `group`.
    pub const USB_DEVICE_TRANSPORT_COMPONENT: u16 = 15;
    /// USBHostTransportComponent — `group`.
    pub const USB_HOST_TRANSPORT_COMPONENT: u16 = 16;
    /// BluetoothTransportComponent — `group`.
    pub const BLUETOOTH_TRANSPORT_COMPONENT: u16 = 17;
    /// iAP2HIDComponent — `group`.
    pub const I_AP2_HID_COMPONENT: u16 = 18;
    /// VehicleInformationComponent — `group`.
    pub const VEHICLE_INFORMATION_COMPONENT: u16 = 20;
    /// VehicleStatusComponent — `group`.
    pub const VEHICLE_STATUS_COMPONENT: u16 = 21;
    /// LocationInformationComponent — `group`.
    pub const LOCATION_INFORMATION_COMPONENT: u16 = 22;
    /// USBHostHIDComponent — `group`.
    pub const USB_HOST_HID_COMPONENT: u16 = 23;
    /// WirelessCarPlayTransportComponent — `group`.
    pub const WIRELESS_CAR_PLAY_TRANSPORT_COMPONENT: u16 = 24;
    /// BLETransportComponent — `group`.
    pub const BLE_TRANSPORT_COMPONENT: u16 = 26;
    /// BluetoothHIDComponent — `group`.
    pub const BLUETOOTH_HID_COMPONENT: u16 = 29;
    /// RouteGuidanceDisplayComponent — `group`.
    pub const ROUTE_GUIDANCE_DISPLAY_COMPONENT: u16 = 30;
    /// RoadObjectDetectionComponent — `group`.
    pub const ROAD_OBJECT_DETECTION_COMPONENT: u16 = 33;
    /// ProductPlanUID — `utf8`.
    pub const PRODUCT_PLAN_UID: u16 = 34;
    /// PhysicalDeviceCompatibility — `utf8`.
    pub const PHYSICAL_DEVICE_COMPATIBILITY: u16 = 35;
    /// Color — `group`.
    pub const COLOR: u16 = 36;
    /// SecondaryColor — `group`.
    pub const SECONDARY_COLOR: u16 = 37;
    /// VehicleDigitalCarKeyInformation — `group`.
    pub const VEHICLE_DIGITAL_CAR_KEY_INFORMATION: u16 = 38;
    /// MaximumCurrentDrawnInUHPM — `uint16`.
    pub const MAXIMUM_CURRENT_DRAWN_IN_UHPM: u16 = 240;
    /// PowerDuringSleep — `enum`.
    pub const POWER_DURING_SLEEP: u16 = 241;

    /// Sub-parameter ids of param 16 (USBHostTransportComponent).
    pub mod usb_host {
        /// TransportComponentIdentifier — `uint16`.
        pub const TRANSPORT_COMPONENT_IDENTIFIER: u16 = 0;
        /// TransportComponentName — `utf8`.
        pub const TRANSPORT_COMPONENT_NAME: u16 = 1;
        /// TransportSupportsiAP2Connection — `none`.
        pub const TRANSPORT_SUPPORTSI_AP2_CONNECTION: u16 = 2;
        /// USBHostTransportCarPlayInterfaceNumber — `uint8`.
        pub const USB_HOST_TRANSPORT_CAR_PLAY_INTERFACE_NUMBER: u16 = 3;
        /// TransportSupportsCarPlay — `none`.
        pub const TRANSPORT_SUPPORTS_CAR_PLAY: u16 = 4;
        /// TransportSupportsThemeAssets — `none`.
        pub const TRANSPORT_SUPPORTS_THEME_ASSETS: u16 = 5;
    }
    /// Sub-parameter ids of param 17 (BluetoothTransportComponent).
    pub mod bluetooth {
        /// TransportComponentIdentifier — `uint16`.
        pub const TRANSPORT_COMPONENT_IDENTIFIER: u16 = 0;
        /// TransportComponentName — `utf8`.
        pub const TRANSPORT_COMPONENT_NAME: u16 = 1;
        /// TransportSupportsiAP2Connection — `none`.
        pub const TRANSPORT_SUPPORTSI_AP2_CONNECTION: u16 = 2;
        /// BluetoothTransportMediaAccessControlAddress — `uint8[6]`.
        pub const BLUETOOTH_TRANSPORT_MEDIA_ACCESS_CONTROL_ADDRESS: u16 = 3;
    }
    /// Sub-parameter ids of param 20 (VehicleInformationComponent).
    pub mod vehicle_info {
        /// Identifier — `uint16`.
        pub const IDENTIFIER: u16 = 0;
        /// Name — `utf8`.
        pub const NAME: u16 = 1;
        /// EngineType — `enum`.
        pub const ENGINE_TYPE: u16 = 2;
        /// EngineType::Gasoline
        pub const ENGINE_TYPE_GASOLINE: u8 = 0;
        /// EngineType::Diesel
        pub const ENGINE_TYPE_DIESEL: u8 = 1;
        /// EngineType::Electric
        pub const ENGINE_TYPE_ELECTRIC: u8 = 2;
        /// EngineType::CNG
        pub const ENGINE_TYPE_CNG: u8 = 3;
        /// DisplayName — `utf8`.
        pub const DISPLAY_NAME: u16 = 6;
        /// MapsDisplayName — `utf8`.
        pub const MAPS_DISPLAY_NAME: u16 = 8;
        /// SiriName — `utf8`.
        pub const SIRI_NAME: u16 = 9;
        /// VehicleColorHexCode — `utf8`.
        pub const VEHICLE_COLOR_HEX_CODE: u16 = 10;
        /// SupportedChargingConnectors — `enum`.
        pub const SUPPORTED_CHARGING_CONNECTORS: u16 = 11;
        /// SupportedChargingConnectors::CCS1
        pub const SUPPORTED_CHARGING_CONNECTORS_CCS1: u8 = 0;
        /// SupportedChargingConnectors::CCS2
        pub const SUPPORTED_CHARGING_CONNECTORS_CCS2: u8 = 1;
        /// SupportedChargingConnectors::J1772
        pub const SUPPORTED_CHARGING_CONNECTORS_J1772: u8 = 2;
        /// SupportedChargingConnectors::CHAdeMO
        pub const SUPPORTED_CHARGING_CONNECTORS_CH_ADE_MO: u8 = 3;
        /// SupportedChargingConnectors::Mennekes
        pub const SUPPORTED_CHARGING_CONNECTORS_MENNEKES: u8 = 4;
        /// SupportedChargingConnectors::GB/T (DC)
        pub const SUPPORTED_CHARGING_CONNECTORS_GB_T_DC_: u8 = 5;
        /// SupportedChargingConnectors::GB/T (AC)
        pub const SUPPORTED_CHARGING_CONNECTORS_GB_T_AC_: u8 = 6;
        /// SupportedChargingConnectors::NACS (DC)
        pub const SUPPORTED_CHARGING_CONNECTORS_NACS_DC_: u8 = 7;
        /// SupportedChargingConnectors::NACS (AC)
        pub const SUPPORTED_CHARGING_CONNECTORS_NACS_AC_: u8 = 8;
        /// PowerForConnectorTypeCCS1 — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_CCS1: u16 = 12;
        /// PowerForConnectorTypeCCS2 — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_CCS2: u16 = 13;
        /// PowerForConnectorTypeJ1772 — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_J1772: u16 = 14;
        /// PowerForConnectorTypeCHAdeMO — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_CH_ADE_MO: u16 = 15;
        /// PowerForConnectorTypeMennekes — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_MENNEKES: u16 = 16;
        /// PowerForConnectorTypeGBT_DC — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_GBT_DC: u16 = 17;
        /// PowerForConnectorTypeGBT_AC — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_GBT_AC: u16 = 18;
        /// PowerForConnectorTypeNACS_DC — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_NACS_DC: u16 = 19;
        /// PowerForConnectorTypeNACS_AC — `uint32`.
        pub const POWER_FOR_CONNECTOR_TYPE_NACS_AC: u16 = 20;
    }
    /// Sub-parameter ids of param 24 (WirelessCarPlayTransportComponent).
    pub mod wireless_carplay {
        /// TransportComponentIdentifier — `uint16`.
        pub const TRANSPORT_COMPONENT_IDENTIFIER: u16 = 0;
        /// TransportComponentName — `utf8`.
        pub const TRANSPORT_COMPONENT_NAME: u16 = 1;
        /// TransportSupportsiAP2Connection — `none`.
        pub const TRANSPORT_SUPPORTSI_AP2_CONNECTION: u16 = 2;
        /// TransportSupportsCarPlay — `none`.
        pub const TRANSPORT_SUPPORTS_CAR_PLAY: u16 = 4;
        /// TransportSupportsThemeAssets — `none`.
        pub const TRANSPORT_SUPPORTS_THEME_ASSETS: u16 = 5;
    }
    /// Sub-parameter ids of param 30 (RouteGuidanceDisplayComponent).
    pub mod route_guidance {
        /// Identifier — `uint16`.
        pub const IDENTIFIER: u16 = 0;
        /// Name — `utf8`.
        pub const NAME: u16 = 1;
        /// MaxCurrentRoadNameLength — `uint16`.
        pub const MAX_CURRENT_ROAD_NAME_LENGTH: u16 = 2;
        /// MaxDestinationNameLength — `uint16`.
        pub const MAX_DESTINATION_NAME_LENGTH: u16 = 3;
        /// MaxAfterManeuverRoadNameLength — `uint16`.
        pub const MAX_AFTER_MANEUVER_ROAD_NAME_LENGTH: u16 = 4;
        /// MaxManeuverDescriptionLength — `uint16`.
        pub const MAX_MANEUVER_DESCRIPTION_LENGTH: u16 = 5;
        /// MaxGuidanceManeuverStorageCapacity — `uint16`.
        pub const MAX_GUIDANCE_MANEUVER_STORAGE_CAPACITY: u16 = 6;
        /// MaxLaneGuidanceDescriptionLength — `uint16`.
        pub const MAX_LANE_GUIDANCE_DESCRIPTION_LENGTH: u16 = 7;
        /// MaxLaneGuidanceStorageCapacity — `uint16`.
        pub const MAX_LANE_GUIDANCE_STORAGE_CAPACITY: u16 = 8;
    }
}

/// Parameter ids inside NowPlayingUpdate (`0x5001`). Apple's own names.
pub mod now_playing {
    /// MediaItemAttributes — `group`.
    pub const MEDIA_ITEM_ATTRIBUTES: u16 = 0;
    /// PlaybackAttributes — `group`.
    pub const PLAYBACK_ATTRIBUTES: u16 = 1;
}

/// Parameter ids inside RouteGuidanceUpdate (`0x5201`). Apple's own names.
pub mod route_guidance_update {
    /// RouteGuidanceDisplayComponentID — `uint16`.
    pub const ROUTE_GUIDANCE_DISPLAY_COMPONENT_ID: u16 = 0;
    /// RouteGuidanceState — `enum`.
    pub const ROUTE_GUIDANCE_STATE: u16 = 1;
    /// ManeuverState — `enum`.
    pub const MANEUVER_STATE: u16 = 2;
    /// CurrentRoadName — `utf8`.
    pub const CURRENT_ROAD_NAME: u16 = 3;
    /// DestinationName — `utf8`.
    pub const DESTINATION_NAME: u16 = 4;
    /// EstimatedTimeOfArrival — `uint64`.
    pub const ESTIMATED_TIME_OF_ARRIVAL: u16 = 5;
    /// TimeRemainingToDestination — `uint64`.
    pub const TIME_REMAINING_TO_DESTINATION: u16 = 6;
    /// DistanceRemaining — `uint32`.
    pub const DISTANCE_REMAINING: u16 = 7;
    /// DistanceRemainingDisplayString — `utf8`.
    pub const DISTANCE_REMAINING_DISPLAY_STRING: u16 = 8;
    /// DistanceRemainingDisplayUnits — `enum`.
    pub const DISTANCE_REMAINING_DISPLAY_UNITS: u16 = 9;
    /// DistanceRemainingToNextManeuver — `uint32`.
    pub const DISTANCE_REMAINING_TO_NEXT_MANEUVER: u16 = 10;
    /// DistanceRemainingToNextManeuverDisplayString — `utf8`.
    pub const DISTANCE_REMAINING_TO_NEXT_MANEUVER_DISPLAY_STRING: u16 = 11;
    /// DistanceRemainingToNextManeuverDisplayUnits — `enum`.
    pub const DISTANCE_REMAINING_TO_NEXT_MANEUVER_DISPLAY_UNITS: u16 = 12;
    /// RouteGuidanceManeuverCurrentList — `uint16[]`.
    pub const ROUTE_GUIDANCE_MANEUVER_CURRENT_LIST: u16 = 13;
    /// RouteGuidanceManeuverTotalCount — `uint16`.
    pub const ROUTE_GUIDANCE_MANEUVER_TOTAL_COUNT: u16 = 14;
    /// RouteGuidanceVisibleInApp — `bool`.
    pub const ROUTE_GUIDANCE_VISIBLE_IN_APP: u16 = 15;
    /// LaneGuidanceCurrentIndex — `uint16`.
    pub const LANE_GUIDANCE_CURRENT_INDEX: u16 = 16;
    /// LaneGuidanceTotalCount — `uint16`.
    pub const LANE_GUIDANCE_TOTAL_COUNT: u16 = 17;
    /// PresentLaneGuidance — `bool`.
    pub const PRESENT_LANE_GUIDANCE: u16 = 18;
    /// SourceName — `utf8`.
    pub const SOURCE_NAME: u16 = 19;
    /// SourceSupportsRouteGuidance — `bool`.
    pub const SOURCE_SUPPORTS_ROUTE_GUIDANCE: u16 = 20;
    /// DestinationTimeZoneOffsetMinutes — `int16`.
    pub const DESTINATION_TIME_ZONE_OFFSET_MINUTES: u16 = 21;
    /// DestinationChargingInfo — `enum`.
    pub const DESTINATION_CHARGING_INFO: u16 = 22;
    /// ChargingStationInfoGroup — `group`.
    pub const CHARGING_STATION_INFO_GROUP: u16 = 23;
    /// ArrivalBatteryLevel — `uint32`.
    pub const ARRIVAL_BATTERY_LEVEL: u16 = 24;
    /// DepartureBatteryLevel — `uint32`.
    pub const DEPARTURE_BATTERY_LEVEL: u16 = 25;
    /// CompletedTripBatteryLevel — `uint32`.
    pub const COMPLETED_TRIP_BATTERY_LEVEL: u16 = 26;
}

/// Parameter ids inside RouteGuidanceManeuverInformation (`0x5202`). Apple's own names.
pub mod route_maneuver {
    /// RouteGuidanceDisplayComponentID — `uint16`.
    pub const ROUTE_GUIDANCE_DISPLAY_COMPONENT_ID: u16 = 0;
    /// Index — `uint16`.
    pub const INDEX: u16 = 1;
    /// ManeuverDescription — `utf8`.
    pub const MANEUVER_DESCRIPTION: u16 = 2;
    /// ManeuverType — `enum`.
    pub const MANEUVER_TYPE: u16 = 3;
    /// AfterManeuverRoadName — `utf8`.
    pub const AFTER_MANEUVER_ROAD_NAME: u16 = 4;
    /// DistanceBetweenManeuver — `uint32`.
    pub const DISTANCE_BETWEEN_MANEUVER: u16 = 5;
    /// DistanceBetweenManeuverDisplayString — `utf8`.
    pub const DISTANCE_BETWEEN_MANEUVER_DISPLAY_STRING: u16 = 6;
    /// DistanceBetweenManeuverDisplayUnits — `enum`.
    pub const DISTANCE_BETWEEN_MANEUVER_DISPLAY_UNITS: u16 = 7;
    /// DrivingSide — `enum`.
    pub const DRIVING_SIDE: u16 = 8;
    /// JunctionType — `enum`.
    pub const JUNCTION_TYPE: u16 = 9;
    /// JunctionElementAngle — `int16`.
    pub const JUNCTION_ELEMENT_ANGLE: u16 = 10;
    /// JunctionElementExitAngle — `int16`.
    pub const JUNCTION_ELEMENT_EXIT_ANGLE: u16 = 11;
    /// LinkedLaneGuidanceInformation — `uint16`.
    pub const LINKED_LANE_GUIDANCE_INFORMATION: u16 = 12;
    /// ExitInfo — `utf8`.
    pub const EXIT_INFO: u16 = 13;
}

/// Parameter ids inside CallStateUpdate (`0x4155`). Apple's own names.
pub mod call_state {
    /// RemoteID — `utf8`.
    pub const REMOTE_ID: u16 = 0;
    /// DisplayName — `utf8`.
    pub const DISPLAY_NAME: u16 = 1;
    /// Status — `enum`.
    pub const STATUS: u16 = 2;
    /// Direction — `enum`.
    pub const DIRECTION: u16 = 3;
    /// CallUUID — `utf8`.
    pub const CALL_UUID: u16 = 4;
    /// AddressBookID — `utf8`.
    pub const ADDRESS_BOOK_ID: u16 = 6;
    /// Label — `utf8`.
    pub const LABEL: u16 = 7;
    /// Service — `enum`.
    pub const SERVICE: u16 = 8;
    /// IsConferenced — `bool`.
    pub const IS_CONFERENCED: u16 = 9;
    /// ConferenceGroup — `uint8`.
    pub const CONFERENCE_GROUP: u16 = 10;
    /// DisconnectReason — `enum`.
    pub const DISCONNECT_REASON: u16 = 11;
    /// StartTimestamp — `secs64`.
    pub const START_TIMESTAMP: u16 = 12;
}

