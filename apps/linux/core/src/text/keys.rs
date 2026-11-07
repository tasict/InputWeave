//! The catalog keys the code uses (strings/*.txt). Every one must be in
//! both catalogs; the tests check `ALL`.

pub const TITLE_UPDATING: &str = "TitleUpdating";
pub const TITLE_STARTING: &str = "TitleStarting";
pub const TITLE_NOT_RESPONDING: &str = "TitleNotResponding";
pub const TITLE_NOT_WORKING: &str = "TitleNotWorking";
pub const TITLE_PAUSED: &str = "TitlePaused";
pub const TITLE_CONTROLLED_BY: &str = "TitleControlledBy";
pub const TITLE_LOCKED_TO: &str = "TitleLockedTo";
pub const TITLE_CONTROLLING: &str = "TitleControlling";
pub const TITLE_SHARING: &str = "TitleSharing";
pub const SUBTITLE_CONTROLLED: &str = "SubtitleControlled";
pub const SUBTITLE_LOCKED: &str = "SubtitleLocked";
pub const SUBTITLE_CONTROLLING: &str = "SubtitleControlling";
pub const SUBTITLE_CONTROLLING_DOUBLE: &str = "SubtitleControllingDouble";
pub const SETUP_TRY_DONE_HINT: &str = "SetupTryDoneHint";
pub const SUBTITLE_CURSOR_ON: &str = "SubtitleCursorOn";
pub const EXCEPTION_SERVICE_STOPPED: &str = "ExceptionServiceStopped";
pub const EXCEPTION_UNREACHABLE: &str = "ExceptionUnreachable";
pub const EXCEPTION_NO_DEVICES: &str = "ExceptionNoDevices";
pub const ACTION_START_SERVICE: &str = "ActionStartService";
pub const TRAY_NORMAL: &str = "TrayNormal";
pub const TRAY_CONTROLLING: &str = "TrayControlling";
pub const TRAY_CONTROLLED: &str = "TrayControlled";
pub const TRAY_PAUSED: &str = "TrayPaused";
pub const TRAY_ATTENTION: &str = "TrayAttention";
pub const MENU_PAUSE_SHARING: &str = "MenuPauseSharing";
pub const MENU_RESUME_SHARING: &str = "MenuResumeSharing";
pub const MENU_LOCK: &str = "MenuLock";
pub const MENU_UNLOCK: &str = "MenuUnlock";
pub const MENU_SETTINGS: &str = "MenuSettings";
pub const MENU_QUIT: &str = "MenuQuit";
pub const QUIT_TITLE: &str = "QuitTitle";
pub const QUIT_MESSAGE: &str = "QuitMessage";
pub const QUIT_CONFIRM: &str = "QuitConfirm";
pub const MENU_SEND_FILES: &str = "MenuSendFiles";
pub const VERBATIM: &str = "Verbatim";
pub const OK: &str = "Ok";
pub const CANCEL: &str = "Cancel";
pub const ACTION_RESTART_SERVICE: &str = "ActionRestartService";
pub const ACTION_TROUBLESHOOT: &str = "ActionTroubleshoot";
pub const ACTION_ADD_DEVICE: &str = "ActionAddDevice";
pub const MENU_ADD_DEVICE: &str = "MenuAddDevice";
pub const TRANSFER_SENDING: &str = "TransferSending";
pub const TRANSFER_RECEIVING: &str = "TransferReceiving";
pub const TRANSFER_PREPARING: &str = "TransferPreparing";
pub const TRANSFER_QUEUED: &str = "TransferQueued";
pub const TRANSFER_WAITING_FOR_CONNECTION: &str = "TransferWaitingForConnection";
pub const TRANSFER_WAITING_FOR_ACCEPT: &str = "TransferWaitingForAccept";
pub const TRANSFER_OFFER: &str = "TransferOffer";
pub const TRANSFER_CHECKING: &str = "TransferChecking";
pub const TRANSFER_SENT: &str = "TransferSent";
pub const TRANSFER_RECEIVED: &str = "TransferReceived";
pub const TRANSFER_CANCELLED: &str = "TransferCancelled";
pub const TRANSFER_PROGRESS: &str = "TransferProgress";
pub const TRANSFER_SECONDS_LEFT: &str = "TransferSecondsLeft";
pub const TRANSFER_MINUTES_LEFT: &str = "TransferMinutesLeft";
pub const TRANSFER_HOURS_LEFT: &str = "TransferHoursLeft";
pub const TRANSFER_PEER_DECLINED: &str = "TransferPeerDeclined";
pub const TRANSFER_PEER_NEEDS_SPACE: &str = "TransferPeerNeedsSpace";
pub const TRANSFER_PEER_DOES_NOT_RECEIVE: &str = "TransferPeerDoesNotReceive";
pub const TRANSFER_PEER_COULD_NOT_TAKE: &str = "TransferPeerCouldNotTake";
pub const TRANSFER_DECLINED: &str = "TransferDeclined";
pub const TRANSFER_NO_SPACE: &str = "TransferNoSpace";
pub const TRANSFER_NOT_RECEIVING: &str = "TransferNotReceiving";
pub const TRANSFER_TOO_MANY: &str = "TransferTooMany";
pub const TRANSFER_UNREADABLE: &str = "TransferUnreadable";
pub const TRANSFER_FILE_CHANGED: &str = "TransferFileChanged";
pub const TRANSFER_CORRUPT: &str = "TransferCorrupt";
pub const TRANSFER_WRITE_FAILED: &str = "TransferWriteFailed";
pub const TRANSFER_FAILED: &str = "TransferFailed";
pub const TRANSFER_ACCEPT: &str = "TransferAccept";
pub const TRANSFER_DECLINE: &str = "TransferDecline";
pub const TRANSFER_CANCEL: &str = "TransferCancel";
pub const TRANSFER_CANCEL_NAMED: &str = "TransferCancelNamed";
pub const TRANSFER_SHOW: &str = "TransferShow";
pub const TRANSFER_SHOW_NAMED: &str = "TransferShowNamed";
pub const TRANSFER_REMOVE: &str = "TransferRemove";
pub const TRANSFER_REMOVE_NAMED: &str = "TransferRemoveNamed";
pub const TRANSFER_FOLDER_FILES: &str = "TransferFolderFiles";
pub const TRANSFER_FOLDER_FILES_ONE: &str = "TransferFolderFilesOne";
pub const TRANSFER_FOLDER_PROGRESS: &str = "TransferFolderProgress";
pub const TRANSFER_SENT_SOME: &str = "TransferSentSome";
pub const TRANSFER_RECEIVED_SOME: &str = "TransferReceivedSome";
pub const TRANSFER_SHOW_FAILURES: &str = "TransferShowFailures";
pub const TRANSFER_HIDE_FAILURES: &str = "TransferHideFailures";
pub const TRANSFER_FAILURE_LINE: &str = "TransferFailureLine";
pub const TRANSFER_FAILURES_MORE: &str = "TransferFailuresMore";
pub const TRANSFER_FOLDER: &str = "TransferFolder";
pub const TRANSFER_SYMLINK: &str = "TransferSymlink";
pub const TRANSFER_NOT_A_FILE: &str = "TransferNotAFile";
pub const TRANSFER_PEER_TOO_LARGE: &str = "TransferPeerTooLarge";
pub const TRANSFER_PEER_INVALID_NAME: &str = "TransferPeerInvalidName";
pub const TRANSFER_INVALID_NAME: &str = "TransferInvalidName";
pub const TRANSFER_NO_SPACE_SHORT: &str = "TransferNoSpaceShort";
pub const CLIPBOARD_SENSITIVE: &str = "ClipboardSensitive";
pub const CLIPBOARD_TOO_LARGE: &str = "ClipboardTooLarge";
pub const CLIPBOARD_SYNCED_FROM: &str = "ClipboardSyncedFrom";
pub const CLIPBOARD_SENT_TO: &str = "ClipboardSentTo";
pub const CLIPBOARD_KIND_IMAGE: &str = "ClipboardKindImage";
pub const CLIPBOARD_KIND_RICH_TEXT: &str = "ClipboardKindRichText";
pub const CLIPBOARD_KIND_TEXT: &str = "ClipboardKindText";
pub const RELATIVE_JUST_NOW: &str = "RelativeJustNow";
pub const RELATIVE_MINUTES_AGO: &str = "RelativeMinutesAgo";
pub const RELATIVE_MINUTES_ONE: &str = "RelativeMinutesOne";
pub const RELATIVE_MINUTES_MANY: &str = "RelativeMinutesMany";
pub const RELATIVE_HOURS_ONE: &str = "RelativeHoursOne";
pub const RELATIVE_HOURS_MANY: &str = "RelativeHoursMany";
pub const RELATIVE_DAYS_ONE: &str = "RelativeDaysOne";
pub const RELATIVE_DAYS_MANY: &str = "RelativeDaysMany";
pub const SEND_FILES_SEND: &str = "SendFilesSend";
pub const SEND_FILES_FAILED: &str = "SendFilesFailed";
pub const SEND_FILES_CHECK_CONNECTED: &str = "SendFilesCheckConnected";
pub const CONNECTION_CONNECTED: &str = "ConnectionConnected";
pub const CONNECTION_CONNECTED_RTT: &str = "ConnectionConnectedRtt";
pub const CONNECTION_SCREEN_LOCKED: &str = "ConnectionScreenLocked";
pub const CONNECTION_SCREEN_LOCKED_RTT: &str = "ConnectionScreenLockedRtt";
pub const CONNECTION_SCREEN_LOCKED_NO_INPUT: &str = "ConnectionScreenLockedNoInput";
pub const CONNECTION_SCREEN_LOCKED_NO_INPUT_RTT: &str = "ConnectionScreenLockedNoInputRtt";
pub const CONNECTION_OFFLINE: &str = "ConnectionOffline";
pub const CONNECTION_OFFLINE_AGO: &str = "ConnectionOfflineAgo";
pub const CONNECTION_WAKING: &str = "ConnectionWaking";
pub const CONNECTION_THIS_COMPUTER: &str = "ConnectionThisComputer";
pub const OS_UNKNOWN: &str = "OsUnknown";
pub const SCREEN_MAIN: &str = "ScreenMain";
pub const SWAP_COMMAND_AND_CONTROL: &str = "SwapCommandAndControl";
pub const REMOVE_DEVICE_TITLE: &str = "RemoveDeviceTitle";
pub const REMOVE_DEVICE_MESSAGE: &str = "RemoveDeviceMessage";
pub const REMOVE_DEVICE_CONFIRM: &str = "RemoveDeviceConfirm";
pub const CANVAS_SYNCED: &str = "CanvasSynced";
pub const ADD_DEVICE_TITLE: &str = "AddDeviceTitle";
pub const PAIRING_PAIR: &str = "PairingPair";
pub const PAIRING_NOT_ACCEPTING: &str = "PairingNotAccepting";
pub const PAIRING_NONE_FOUND: &str = "PairingNoneFound";
pub const PAIRING_NONE_FOUND_DETAIL: &str = "PairingNoneFoundDetail";
pub const PAIRING_IP_ADDRESS: &str = "PairingIpAddress";
pub const PAIRING_ADD_BY_IP: &str = "PairingAddByIp";
pub const PAIRING_CONTINUE: &str = "PairingContinue";
pub const PAIRING_START_OVER: &str = "PairingStartOver";
pub const PAIRING_CODE_NAME: &str = "PairingCodeName";
pub const PAIRING_CODE_TITLE: &str = "PairingCodeTitle";
pub const PAIRING_CODE_VALID: &str = "PairingCodeValid";
pub const PAIRING_DECLINE: &str = "PairingDecline";
pub const PAIRING_CONNECTING: &str = "PairingConnecting";
pub const PAIRING_ENTER_CODE: &str = "PairingEnterCode";
pub const PAIRING_WRONG_CODE: &str = "PairingWrongCode";
pub const PAIRING_PAIRED: &str = "PairingPaired";
pub const PAIRING_THE_OTHER_COMPUTER: &str = "PairingTheOtherComputer";
pub const PAIRING_TOO_MANY_ATTEMPTS: &str = "PairingTooManyAttempts";
pub const PAIRING_EXPIRED: &str = "PairingExpired";
pub const PAIRING_REJECTED: &str = "PairingRejected";
pub const PAIRING_NOT_ALLOWED: &str = "PairingNotAllowed";
pub const PAIRING_BUSY: &str = "PairingBusy";
pub const PAIRING_ID_CONFLICT: &str = "PairingIdConflict";
pub const PAIRING_INCOMPATIBLE: &str = "PairingIncompatible";
pub const PAIRING_UNREACHABLE: &str = "PairingUnreachable";
pub const PAIRING_CANCELLED: &str = "PairingCancelled";
pub const PAIRING_DID_NOT_WORK: &str = "PairingDidNotWork";
pub const PAIRING_START_BUSY: &str = "PairingStartBusy";
pub const PAIRING_START_INVALID_ADDRESS: &str = "PairingStartInvalidAddress";
pub const PAIRING_START_UNREACHABLE: &str = "PairingStartUnreachable";
pub const PAIRING_START_FAILED: &str = "PairingStartFailed";
pub const NAV_ARRANGEMENT: &str = "NavArrangement";
pub const NAV_DEVICES: &str = "NavDevices";
pub const NAV_FILES: &str = "NavFiles";
pub const NAV_GENERAL: &str = "NavGeneral";
pub const NAV_TROUBLESHOOTING: &str = "NavTroubleshooting";
pub const MAIN_UNAVAILABLE: &str = "MainUnavailable";
pub const INSPECTOR_NAME: &str = "InspectorName";
pub const INSPECTOR_SYSTEM: &str = "InspectorSystem";
pub const INSPECTOR_STATUS: &str = "InspectorStatus";
pub const INSPECTOR_SCREENS: &str = "InspectorScreens";
pub const INSPECTOR_SCREENS_LATER: &str = "InspectorScreensLater";
pub const REMOVE_DEVICE: &str = "RemoveDevice";
pub const DEVICES_NONE_TITLE: &str = "DevicesNoneTitle";
pub const DEVICES_NONE_DETAIL: &str = "DevicesNoneDetail";
pub const FILES_RECEIVING: &str = "FilesReceiving";
pub const FILES_SAVE_IN: &str = "FilesSaveIn";
pub const FILES_CHOOSE: &str = "FilesChoose";
pub const FILES_WHEN_SENT: &str = "FilesWhenSent";
pub const FILES_AUTOMATICALLY: &str = "FilesAutomatically";
pub const FILES_ASK_EACH_TIME: &str = "FilesAskEachTime";
pub const FILES_SENDING: &str = "FilesSending";
pub const FILES_SEND_HINT: &str = "FilesSendHint";
pub const FILES_SEND_BUTTON: &str = "FilesSendButton";
pub const FILES_RECENT: &str = "FilesRecent";
pub const FILES_NONE: &str = "FilesNone";
pub const FILES_CLEAR_HISTORY: &str = "FilesClearHistory";
pub const FILES_CLEAR_TITLE: &str = "FilesClearTitle";
pub const FILES_CLEAR_MESSAGE: &str = "FilesClearMessage";
pub const FILES_CLEAR_CONFIRM: &str = "FilesClearConfirm";
pub const GENERAL_NAME: &str = "GeneralName";
pub const GENERAL_NAME_HINT: &str = "GeneralNameHint";
pub const GENERAL_OPEN_AT_LOGIN: &str = "GeneralOpenAtLogin";
pub const GENERAL_CLIPBOARD: &str = "GeneralClipboard";
pub const GENERAL_CLIPBOARD_SYNC: &str = "GeneralClipboardSync";
pub const GENERAL_CLIPBOARD_HINT: &str = "GeneralClipboardHint";
pub const GENERAL_PICTURES: &str = "GeneralPictures";
pub const GENERAL_PICTURES_SHARE: &str = "GeneralPicturesShare";
pub const GENERAL_PICTURES_HINT: &str = "GeneralPicturesHint";
pub const GENERAL_PICTURES_UNAVAILABLE: &str = "GeneralPicturesUnavailable";
pub const GENERAL_SWITCHING: &str = "GeneralSwitching";
pub const GENERAL_WAIT: &str = "GeneralWait";
pub const GENERAL_WAIT_HINT: &str = "GeneralWaitHint";
pub const GENERAL_SHORTCUTS: &str = "GeneralShortcuts";
pub const SHORTCUT_TAKE_BACK: &str = "ShortcutTakeBack";
pub const SHORTCUT_LOCK: &str = "ShortcutLock";
pub const SHORTCUT_JUMP: &str = "ShortcutJump";
pub const SHORTCUT_OFF: &str = "ShortcutOff";
pub const SHORTCUT_RESET: &str = "ShortcutReset";
pub const SHORTCUTS_HINT: &str = "ShortcutsHint";
pub const SWITCH_MODIFIER_TITLE: &str = "SwitchModifierTitle";
pub const SWITCH_MODIFIER_HINT: &str = "SwitchModifierHint";
pub const SWITCH_ALWAYS: &str = "SwitchAlways";
pub const SWITCH_HOLDING_CONTROL: &str = "SwitchHoldingControl";
pub const SWITCH_HOLDING_ALT: &str = "SwitchHoldingAlt";
pub const SWITCH_HOLDING_SHIFT: &str = "SwitchHoldingShift";
pub const IDENTIFY_SCREENS: &str = "IdentifyScreens";
pub const IDENTIFY_SCREENS_HINT: &str = "IdentifyScreensHint";
pub const CANVAS_RENAME: &str = "CanvasRename";
pub const INSPECTOR_CALLS_ITSELF: &str = "InspectorCallsItself";
pub const INSPECTOR_SYNC_CLIPBOARD: &str = "InspectorSyncClipboard";
pub const INSPECTOR_SEND_FILES: &str = "InspectorSendFiles";
pub const INSPECTOR_SHARING_HINT: &str = "InspectorSharingHint";
pub const INSPECTOR_CURSOR_SPEED: &str = "InspectorCursorSpeed";
pub const INSPECTOR_CURSOR_SPEED_HINT: &str = "InspectorCursorSpeedHint";
pub const INSPECTOR_CURSOR_SPEED_RESET: &str = "InspectorCursorSpeedReset";
pub const WAKE: &str = "Wake";
pub const WAKE_NOT_WOKEN: &str = "WakeNotWoken";
pub const WAKE_HOW_TO: &str = "WakeHowTo";
pub const WAKE_HOW_TO_URL: &str = "WakeHowToUrl";
pub const WAKE_NOT_TOLD_YET: &str = "WakeNotToldYet";
pub const DEVICE_FILES_OFF: &str = "DeviceFilesOff";
pub const DEVICE_FILES_OFF_SHORT: &str = "DeviceFilesOffShort";
pub const TROUBLE_PROBLEMS: &str = "TroubleProblems";
pub const TROUBLE_ALL_GOOD: &str = "TroubleAllGood";
pub const TROUBLE_SERVICE: &str = "TroubleService";
pub const TROUBLE_VERSION: &str = "TroubleVersion";
pub const TROUBLE_BUILD: &str = "TroubleBuild";
pub const TROUBLE_KEYBOARD_MOUSE: &str = "TroubleKeyboardMouse";
pub const TROUBLE_RESTART_HINT: &str = "TroubleRestartHint";
pub const TROUBLE_DEVICE_ID: &str = "TroubleDeviceId";
pub const TROUBLE_EXPORT: &str = "TroubleExport";
pub const TROUBLE_EXPORT_HINT: &str = "TroubleExportHint";
pub const NAME_INVALID: &str = "NameInvalid";
pub const SERVICE_RUNNING: &str = "ServiceRunning";
pub const SERVICE_STARTING: &str = "ServiceStarting";
pub const SERVICE_UPDATING: &str = "ServiceUpdating";
pub const SERVICE_STOPPING: &str = "ServiceStopping";
pub const SERVICE_STOPPED: &str = "ServiceStopped";
pub const SERVICE_NOT_INSTALLED: &str = "ServiceNotInstalled";
pub const CAN_CONTROL: &str = "CanControl";
pub const CANNOT_CONTROL_YET: &str = "CannotControlYet";
pub const DIAGNOSTICS_FILE_NAME: &str = "DiagnosticsFileName";
pub const DIAGNOSTICS_FAILED: &str = "DiagnosticsFailed";
pub const NOTIFY_RECEIVED_ONE: &str = "NotifyReceivedOne";
pub const NOTIFY_RECEIVED_MANY: &str = "NotifyReceivedMany";
pub const NOTIFY_OPEN_FOLDER: &str = "NotifyOpenFolder";
pub const NOTIFY_OFFER_TITLE: &str = "NotifyOfferTitle";
pub const NOTIFY_OFFER_BODY: &str = "NotifyOfferBody";
pub const NOTIFY_SERVICE_STOPPED: &str = "NotifyServiceStopped";
pub const NOTIFY_LIST_SEPARATOR: &str = "NotifyListSeparator";
pub const NOTIFY_RECEIVED_ITEMS: &str = "NotifyReceivedItems";
pub const NOTIFY_FAILED_ONE: &str = "NotifyFailedOne";
pub const NOTIFY_FAILED_MANY: &str = "NotifyFailedMany";
pub const ITEMS_FOLDER_ONE: &str = "ItemsFolderOne";
pub const ITEMS_FOLDER_MANY: &str = "ItemsFolderMany";
pub const ITEMS_FILE_ONE: &str = "ItemsFileOne";
pub const ITEMS_FILE_MANY: &str = "ItemsFileMany";
pub const ITEMS_AND: &str = "ItemsAnd";
pub const SEND_PICK_TITLE: &str = "SendPickTitle";
pub const SEND_SUMMARY_ONE_MORE: &str = "SendSummaryOneMore";
pub const SEND_SUMMARY_MORE: &str = "SendSummaryMore";
pub const SEND_REASON_FILES_OFF: &str = "SendReasonFilesOff";
pub const SEND_NOWHERE: &str = "SendNowhere";
pub const SEND_FILES_LIMITS: &str = "SendFilesLimits";
pub const HUD_SENDING_ONE: &str = "HudSendingOne";
pub const HUD_SENDING_MANY: &str = "HudSendingMany";
pub const TITLE_RELATIVE: &str = "TitleRelative";
pub const SUBTITLE_RELATIVE: &str = "SubtitleRelative";
pub const GENERAL_RELATIVE_AUTO: &str = "GeneralRelativeAuto";
pub const GENERAL_RELATIVE_AUTO_HINT: &str = "GeneralRelativeAutoHint";
pub const GENERAL_RELATIVE_AUTO_HINT_NO_SHORTCUT: &str = "GeneralRelativeAutoHintNoShortcut";
pub const GENERAL_RELATIVE_AUTO_FAILED: &str = "GeneralRelativeAutoFailed";
pub const SHORTCUT_RELATIVE: &str = "ShortcutRelative";
pub const SWAP_SUPER_AND_CONTROL: &str = "SwapSuperAndControl";
pub const SWITCH_HOLDING_SUPER: &str = "SwitchHoldingSuper";
pub const STATUS_HEADER: &str = "StatusHeader";
pub const GENERAL_SHARING: &str = "GeneralSharing";
pub const ARRANGEMENT_DEVICES: &str = "ArrangementDevices";
pub const ARRANGEMENT_PLACED_AT: &str = "ArrangementPlacedAt";
pub const ARRANGEMENT_NOT_PLACED: &str = "ArrangementNotPlaced";
pub const PAIRING_NEARBY: &str = "PairingNearby";
pub const CLOSE: &str = "Close";
pub const SERVICE_START_FAILED: &str = "ServiceStartFailed";
pub const SIZE_BYTE: &str = "SizeByte";
pub const SIZE_BYTES: &str = "SizeBytes";
pub const FILES_CHOOSE_TITLE: &str = "FilesChooseTitle";
pub const FILES_SETTINGS_FAILED: &str = "FilesSettingsFailed";
pub const SEND_FILES_CHOOSE_TITLE: &str = "SendFilesChooseTitle";
pub const SEND_FILES_CHOOSE_FOLDERS: &str = "SendFilesChooseFolders";
pub const SEND_FILES_FOLDERS: &str = "SendFilesFolders";
pub const SHORTCUT_JUMP_OFF: &str = "ShortcutJumpOff";
pub const SHORTCUT_RELATIVE_OFF: &str = "ShortcutRelativeOff";
pub const SHORTCUTS_RECORD_LATER: &str = "ShortcutsRecordLater";
pub const TRAY_NO_HOST_TITLE: &str = "TrayNoHostTitle";
pub const DRAG_NOT_HERE: &str = "DragNotHere";
pub const NAME_CHANGE_FAILED: &str = "NameChangeFailed";
pub const COMMAND_FAILED: &str = "CommandFailed";
pub const PAIRING_CODE_ENTRY: &str = "PairingCodeEntry";
pub const NOTIFY_OFFER_MANY: &str = "NotifyOfferMany";
pub const KEYBOARD_MOUSE_HERE: &str = "KeyboardMouseHere";
pub const SCREEN_NUMBERED: &str = "ScreenNumbered";
pub const PAIRING_SEND_AGAIN: &str = "PairingSendAgain";
pub const SERVICE_RESTART_FAILED: &str = "ServiceRestartFailed";

/// Every key above.
pub const CANVAS_DROP_TO_SEND: &str = "CanvasDropToSend";
pub const CANVAS_MOVE_CURSOR_HERE: &str = "CanvasMoveCursorHere";
pub const CANVAS_LABEL: &str = "CanvasLabel";
pub const SHORTCUT_RECORD: &str = "ShortcutRecord";
pub const SHORTCUT_PRESS_KEYS: &str = "ShortcutPressKeys";
pub const SHORTCUT_PRESS_SINGLE: &str = "ShortcutPressSingle";
pub const SHORTCUT_SINGLE_KEYS: &str = "ShortcutSingleKeys";
pub const SHORTCUT_DOUBLE_PRESS: &str = "ShortcutDoublePress";
pub const SHORTCUT_DOUBLE_PRESS_HINT: &str = "ShortcutDoublePressHint";
pub const SHORTCUT_PRESS_JUMP: &str = "ShortcutPressJump";
pub const SHORTCUT_NEEDS_MODIFIER: &str = "ShortcutNeedsModifier";
pub const SHORTCUT_UNKNOWN_KEY: &str = "ShortcutUnknownKey";
pub const SHORTCUT_USED_FOR: &str = "ShortcutUsedFor";
pub const SHORTCUT_USED_ELSEWHERE: &str = "ShortcutUsedElsewhere";
pub const SHORTCUT_NOT_CHANGED: &str = "ShortcutNotChanged";
pub const SHORTCUT_NOT_SAVED: &str = "ShortcutNotSaved";
pub const SHORTCUT_TURN_ON: &str = "ShortcutTurnOn";
pub const MENU_RUN_SETUP: &str = "MenuRunSetup";
pub const SETUP_WINDOW_TITLE: &str = "SetupWindowTitle";
pub const SETUP_STEP_OF: &str = "SetupStepOf";
pub const SETUP_BACK: &str = "SetupBack";
pub const SETUP_LATER: &str = "SetupLater";
pub const SETUP_CONTINUE: &str = "SetupContinue";
pub const SETUP_DONE: &str = "SetupDone";
pub const SETUP_SKIP: &str = "SetupSkip";
pub const SETUP_GET_STARTED: &str = "SetupGetStarted";
pub const SETUP_WELCOME_TITLE: &str = "SetupWelcomeTitle";
pub const SETUP_BENEFIT_EDGES: &str = "SetupBenefitEdges";
pub const SETUP_BENEFIT_CLIPBOARD: &str = "SetupBenefitClipboard";
pub const SETUP_BENEFIT_FILES: &str = "SetupBenefitFiles";
pub const SETUP_REASSURANCE: &str = "SetupReassurance";
pub const SETUP_NAME_TITLE: &str = "SetupNameTitle";
pub const SETUP_NAME_LABEL: &str = "SetupNameLabel";
pub const SETUP_NAME_HINT: &str = "SetupNameHint";
pub const SETUP_PERMISSIONS_TITLE: &str = "SetupPermissionsTitle";
pub const SETUP_CONNECT_TITLE: &str = "SetupConnectTitle";
pub const SETUP_CONNECT_HINT: &str = "SetupConnectHint";
pub const SETUP_CONNECT_BUTTON: &str = "SetupConnectButton";
pub const SETUP_CONNECTED: &str = "SetupConnected";
pub const SETUP_ARRANGE_TITLE: &str = "SetupArrangeTitle";
pub const SETUP_ARRANGE_HINT: &str = "SetupArrangeHint";
pub const SETUP_ARRANGE_ALONE: &str = "SetupArrangeAlone";
pub const SETUP_TRY_TITLE: &str = "SetupTryTitle";
pub const SETUP_TRY_HINT: &str = "SetupTryHint";
pub const SETUP_TRY_DONE: &str = "SetupTryDone";
pub const SETUP_TRY_ALONE: &str = "SetupTryAlone";
pub const HUD_TAKE_BACK_HINT: &str = "HudTakeBackHint";
pub const HUD_LOCKED_HERE: &str = "HudLockedHere";
pub const HUD_LOCKED_TO: &str = "HudLockedTo";
pub const HUD_UNLOCKED: &str = "HudUnlocked";
pub const HUD_PEER_LOST: &str = "HudPeerLost";
pub const HUD_REMOTE_ACTIVITY: &str = "HudRemoteActivity";
pub const HUD_BACK_HERE: &str = "HudBackHere";
pub const HUD_LAYOUT_CHANGED: &str = "HudLayoutChanged";
pub const HUD_SCREEN_LOCKED: &str = "HudScreenLocked";
pub const HUD_TWO_LINES: &str = "HudTwoLines";
pub const HUD_RELATIVE_ENTERED: &str = "HudRelativeEntered";
pub const HUD_RELATIVE_ENTERED_TAKE_BACK: &str = "HudRelativeEnteredTakeBack";
pub const HUD_RELATIVE_ENDED: &str = "HudRelativeEnded";
pub const HUD_RELATIVE_RELEASED: &str = "HudRelativeReleased";
pub const HUD_RELATIVE_UNSUPPORTED: &str = "HudRelativeUnsupported";
pub const CONNECTION_PEER_PAUSED: &str = "ConnectionPeerPaused";
pub const CONNECTION_PEER_PAUSED_RTT: &str = "ConnectionPeerPausedRtt";
pub const CONNECTION_NO_INPUT: &str = "ConnectionNoInput";
pub const CONNECTION_NO_INPUT_RTT: &str = "ConnectionNoInputRtt";
pub const HUD_PEER_PAUSED: &str = "HudPeerPaused";
pub const HUD_CAPTURE_ENDED: &str = "HudCaptureEnded";
pub const HUD_INPUT_UNAVAILABLE: &str = "HudInputUnavailable";
pub const HUD_WAKING: &str = "HudWaking";
pub const HUD_AWAKE: &str = "HudAwake";
pub const SUBTITLE_CONTROLLED_WAYLAND: &str = "SubtitleControlledWayland";
pub const GENERAL_EDGE_GLOW: &str = "GeneralEdgeGlow";
pub const GENERAL_EDGE_GLOW_HINT: &str = "GeneralEdgeGlowHint";
pub const GENERAL_EDGE_GLOW_NONE: &str = "GeneralEdgeGlowNone";
pub const GENERAL_WAKE_ON_PUSH: &str = "GeneralWakeOnPush";
pub const GENERAL_WAKE_ON_PUSH_HINT: &str = "GeneralWakeOnPushHint";
pub const GENERAL_WAKE_ON_PUSH_EDGES_ONLY: &str = "GeneralWakeOnPushEdgesOnly";
pub const EDGES_ONLY: &str = "EdgesOnly";
pub const TROUBLE_SESSION: &str = "TroubleSession";
pub const TROUBLE_SESSION_HINT: &str = "TroubleSessionHint";
pub const TROUBLE_SESSION_TYPE: &str = "TroubleSessionType";
pub const SESSION_UNKNOWN: &str = "SessionUnknown";
pub const ROLE_INPUT: &str = "RoleInput";
pub const ROLE_CAPTURE: &str = "RoleCapture";
pub const ROLE_CLIPBOARD: &str = "RoleClipboard";
pub const ROLE_DISPLAYS: &str = "RoleDisplays";
pub const ROLE_DESKTOP_PICTURES: &str = "RoleDesktopPictures";
pub const BACKEND_WITH_PROBLEM: &str = "BackendWithProblem";
pub const BACKEND_NONE: &str = "BackendNone";
pub const CLIPBOARD_NEEDS_REMOTE_CONTROL: &str = "ClipboardNeedsRemoteControl";
pub const TROUBLE_FIREWALL: &str = "TroubleFirewall";
pub const FIREWALL_BLOCKS_PORT: &str = "FirewallBlocksPort";
pub const FIREWALL_BLOCKS_MDNS: &str = "FirewallBlocksMdns";
pub const FIREWALL_BLOCKS_BOTH: &str = "FirewallBlocksBoth";
pub const FIREWALL_UNREADABLE: &str = "FirewallUnreadable";
pub const FIREWALL_COMMAND: &str = "FirewallCommand";
pub const COPY: &str = "Copy";
pub const COPIED: &str = "Copied";
pub const TROUBLE_PERMISSIONS: &str = "TroublePermissions";
pub const PERMISSIONS_HINT: &str = "PermissionsHint";
pub const PERMISSION_REMOTE_CONTROL: &str = "PermissionRemoteControl";
pub const PERMISSION_INPUT_CAPTURE: &str = "PermissionInputCapture";
pub const PERMISSION_GRANTED: &str = "PermissionGranted";
pub const PERMISSION_NOT_ASKED: &str = "PermissionNotAsked";
pub const PERMISSION_ASKING: &str = "PermissionAsking";
pub const PERMISSION_DENIED: &str = "PermissionDenied";
pub const PERMISSION_FAILED: &str = "PermissionFailed";
pub const PERMISSION_FAILED_PLAIN: &str = "PermissionFailedPlain";
pub const PERMISSION_RESTORING: &str = "PermissionRestoring";
pub const PERMISSION_STOPPED: &str = "PermissionStopped";
pub const PERMISSION_EVERY_LOGIN: &str = "PermissionEveryLogin";
pub const PERMISSION_ALLOW: &str = "PermissionAllow";
pub const PERMISSION_ALLOW_AGAIN: &str = "PermissionAllowAgain";
pub const TROUBLE_JOURNAL: &str = "TroubleJournal";
pub const TROUBLE_JOURNAL_HINT: &str = "TroubleJournalHint";
pub const TROUBLE_JOURNAL_APP: &str = "TroubleJournalApp";
pub const GENERAL_EDGE_GLOW_UNAVAILABLE: &str = "GeneralEdgeGlowUnavailable";
pub const TITLE_APP_OLDER: &str = "TitleAppOlder";
pub const APP_OLDER_HINT: &str = "AppOlderHint";

// The license and the updates.
pub const LICENSE_NAV: &str = "LicenseNav";
pub const LICENSE_TRIAL: &str = "LicenseTrial";
pub const LICENSE_LICENSED: &str = "LicenseLicensed";
pub const LICENSE_TRIAL_BODY: &str = "LicenseTrialBody";
pub const LICENSE_FULL_BODY: &str = "LicenseFullBody";
pub const LICENSE_INVALID_BODY: &str = "LicenseInvalidBody";
pub const LICENSE_UNAVAILABLE_BODY: &str = "LicenseUnavailableBody";
pub const LICENSE_PRICE: &str = "LicensePrice";
pub const LICENSE_BUY: &str = "LicenseBuy";
pub const LICENSE_ENTER: &str = "LicenseEnter";
pub const LICENSE_MANAGE: &str = "LicenseManage";
pub const LICENSE_TRY_AGAIN: &str = "LicenseTryAgain";
pub const LICENSE_HOW_TITLE: &str = "LicenseHowTitle";
pub const LICENSE_HOW_BUY: &str = "LicenseHowBuy";
pub const LICENSE_HOW_ENTER: &str = "LicenseHowEnter";
pub const LICENSE_HOW_PAIRED: &str = "LicenseHowPaired";
pub const LICENSE_POLAR_NOTE: &str = "LicensePolarNote";
pub const LICENSE_LICENSED_TO: &str = "LicenseLicensedTo";
pub const LICENSE_THANKS: &str = "LicenseThanks";
pub const LICENSE_FROM_PAIRED: &str = "LicenseFromPaired";
pub const LICENSE_COMPUTERS: &str = "LicenseComputers";
pub const LICENSE_COUNT: &str = "LicenseCount";
pub const LICENSE_THIS_COMPUTER: &str = "LicenseThisComputer";
pub const LICENSE_PAIRED: &str = "LicensePaired";
pub const LICENSE_LIST_NOTE: &str = "LicenseListNote";
pub const LICENSE_REMOVE: &str = "LicenseRemove";
pub const LICENSE_ENTER_TITLE: &str = "LicenseEnterTitle";
pub const LICENSE_KEY_LABEL: &str = "LicenseKeyLabel";
pub const LICENSE_KEY_HELP: &str = "LicenseKeyHelp";
pub const LICENSE_ACTIVATE: &str = "LicenseActivate";
pub const LICENSE_ACTIVATING: &str = "LicenseActivating";
pub const LICENSE_ERR_INVALID: &str = "LicenseErrInvalid";
pub const LICENSE_ERR_FULL: &str = "LicenseErrFull";
pub const LICENSE_ERR_OFFLINE: &str = "LicenseErrOffline";
pub const LICENSE_REMOVE_TITLE: &str = "LicenseRemoveTitle";
pub const LICENSE_REMOVE_BODY: &str = "LicenseRemoveBody";
pub const LICENSE_REMOVE_CONFIRM: &str = "LicenseRemoveConfirm";
pub const LICENSE_REMOVED_OFFLINE: &str = "LicenseRemovedOffline";
pub const MENU_ABOUT: &str = "MenuAbout";
pub const UPDATES_TITLE: &str = "UpdatesTitle";
pub const ABOUT_GPL: &str = "AboutGpl";
pub const LICENSE_ERR_RECORD: &str = "LicenseErrRecord";
pub const LICENSE_REMOVING: &str = "LicenseRemoving";
pub const LICENSE_REMOVE_FAILED: &str = "LicenseRemoveFailed";
pub const LICENSE_ERR_UNAVAILABLE: &str = "LicenseErrUnavailable";
pub const UPDATES_VERSION: &str = "UpdatesVersion";
pub const UPDATES_BODY: &str = "UpdatesBody";
pub const UPDATES_SOURCE: &str = "UpdatesSource";
pub const UPDATES_NOTE: &str = "UpdatesNote";
pub const CONNECTION_OFFLINE_FOR_NOW: &str = "ConnectionOfflineForNow";
pub const OFFLINE_SET_BY: &str = "OfflineSetBy";
pub const OFFLINE_SET_HERE: &str = "OfflineSetHere";
pub const OFFLINE_UNAWARE: &str = "OfflineUnaware";
pub const OFFLINE_TAKE: &str = "OfflineTake";
pub const OFFLINE_BRING_BACK: &str = "OfflineBringBack";
pub const OFFLINE_EXPLAIN: &str = "OfflineExplain";
pub const MENU_TAKE_THIS_OFFLINE: &str = "MenuTakeThisOffline";
pub const TITLE_THIS_OFFLINE: &str = "TitleThisOffline";
pub const SUBTITLE_THIS_OFFLINE_BY: &str = "SubtitleThisOfflineBy";
pub const SUBTITLE_THIS_OFFLINE_HERE: &str = "SubtitleThisOfflineHere";
pub const TRAY_THIS_OFFLINE: &str = "TrayThisOffline";
pub const HUD_OFFLINE: &str = "HudOffline";
pub const HUD_BACK_ONLINE: &str = "HudBackOnline";
pub const HUD_OFFLINE_RETURNED: &str = "HudOfflineReturned";
pub const HUD_TOOK_THIS_OFFLINE: &str = "HudTookThisOffline";
pub const OFFLINE_SET_BY_UNKNOWN: &str = "OfflineSetByUnknown";
pub const SUBTITLE_THIS_OFFLINE_BY_UNKNOWN: &str = "SubtitleThisOfflineByUnknown";
pub const HUD_TOOK_THIS_OFFLINE_UNKNOWN: &str = "HudTookThisOfflineUnknown";
pub const NAV_PROFILES: &str = "NavProfiles";
pub const PROFILE_IN_USE: &str = "ProfileInUse";
pub const PROFILE_IN_USE_HINT: &str = "ProfileInUseHint";
pub const PROFILE_AUTOMATIC: &str = "ProfileAutomatic";
pub const PROFILE_AUTOMATIC_NOW: &str = "ProfileAutomaticNow";
pub const PROFILE_DEFAULT: &str = "ProfileDefault";
pub const PROFILE_OTHER_NETWORKS: &str = "ProfileOtherNetworks";
pub const PROFILE_NEW_NAME: &str = "ProfileNewName";
pub const PROFILE_CURRENT_NETWORK: &str = "ProfileCurrentNetwork";
pub const PROFILE_NO_NETWORK: &str = "ProfileNoNetwork";
pub const NETWORK_ROUTER: &str = "NetworkRouter";
pub const NETWORK_VPN: &str = "NetworkVpn";
pub const NETWORK_WIRED: &str = "NetworkWired";
pub const NETWORK_WIFI: &str = "NetworkWifi";
pub const PROFILE_NAME_TAKEN: &str = "ProfileNameTaken";
pub const PROFILE_ADD_TO: &str = "ProfileAddTo";
pub const PROFILE_IN_USE_TAG: &str = "ProfileInUseTag";
pub const PROFILE_ADD: &str = "ProfileAdd";
pub const PROFILE_REMOVE: &str = "ProfileRemove";
pub const PROFILE_SHARING: &str = "ProfileSharing";
pub const PROFILE_SHARE: &str = "ProfileShare";
pub const PROFILE_PAUSE: &str = "ProfilePause";
pub const PROFILE_PAUSED_HINT: &str = "ProfilePausedHint";
pub const PROFILE_COMPUTERS: &str = "ProfileComputers";
pub const PROFILE_COMPUTERS_HINT: &str = "ProfileComputersHint";
pub const PROFILE_FILES: &str = "ProfileFiles";
pub const PROFILE_CLIPBOARD_HINT: &str = "ProfileClipboardHint";
pub const PROFILE_NETWORKS: &str = "ProfileNetworks";
pub const PROFILE_NO_NETWORKS: &str = "ProfileNoNetworks";
pub const PROFILE_OTHER_NETWORKS_HINT: &str = "ProfileOtherNetworksHint";
pub const PROFILE_ADD_CURRENT_NETWORK: &str = "ProfileAddCurrentNetwork";
pub const PROFILE_REMOVE_NETWORK: &str = "ProfileRemoveNetwork";
pub const MENU_PROFILE: &str = "MenuProfile";
pub const MENU_MANAGE_PROFILES: &str = "MenuManageProfiles";
pub const TITLE_PROFILE_PAUSED: &str = "TitleProfilePaused";
pub const PROFILE_NETWORK_NOT_IN_ANY: &str = "ProfileNetworkNotInAny";
pub const PROFILE_ADD_THIS_NETWORK: &str = "ProfileAddThisNetwork";
pub const CONNECTION_NOT_IN_PROFILE: &str = "ConnectionNotInProfile";
pub const HUD_PROFILE_SWITCHED: &str = "HudProfileSwitched";
pub const HUD_PROFILE_SWITCHED_PAUSED: &str = "HudProfileSwitchedPaused";

pub const ALL: &[&str] = &[
    TITLE_UPDATING,
    TITLE_STARTING,
    TITLE_NOT_RESPONDING,
    TITLE_NOT_WORKING,
    TITLE_PAUSED,
    TITLE_CONTROLLED_BY,
    TITLE_LOCKED_TO,
    TITLE_CONTROLLING,
    TITLE_SHARING,
    SUBTITLE_CONTROLLED,
    SUBTITLE_LOCKED,
    SUBTITLE_CONTROLLING,
    SUBTITLE_CONTROLLING_DOUBLE,
    SETUP_TRY_DONE_HINT,
    SUBTITLE_CURSOR_ON,
    EXCEPTION_SERVICE_STOPPED,
    EXCEPTION_UNREACHABLE,
    EXCEPTION_NO_DEVICES,
    ACTION_START_SERVICE,
    TRAY_NORMAL,
    TRAY_CONTROLLING,
    TRAY_CONTROLLED,
    TRAY_PAUSED,
    TRAY_ATTENTION,
    MENU_PAUSE_SHARING,
    MENU_RESUME_SHARING,
    MENU_LOCK,
    MENU_UNLOCK,
    MENU_SETTINGS,
    MENU_QUIT,
    QUIT_TITLE,
    QUIT_MESSAGE,
    QUIT_CONFIRM,
    MENU_SEND_FILES,
    VERBATIM,
    OK,
    CANCEL,
    ACTION_RESTART_SERVICE,
    ACTION_TROUBLESHOOT,
    ACTION_ADD_DEVICE,
    MENU_ADD_DEVICE,
    TRANSFER_SENDING,
    TRANSFER_RECEIVING,
    TRANSFER_PREPARING,
    TRANSFER_QUEUED,
    TRANSFER_WAITING_FOR_CONNECTION,
    TRANSFER_WAITING_FOR_ACCEPT,
    TRANSFER_OFFER,
    TRANSFER_CHECKING,
    TRANSFER_SENT,
    TRANSFER_RECEIVED,
    TRANSFER_CANCELLED,
    TRANSFER_PROGRESS,
    TRANSFER_SECONDS_LEFT,
    TRANSFER_MINUTES_LEFT,
    TRANSFER_HOURS_LEFT,
    TRANSFER_PEER_DECLINED,
    TRANSFER_PEER_NEEDS_SPACE,
    TRANSFER_PEER_DOES_NOT_RECEIVE,
    TRANSFER_PEER_COULD_NOT_TAKE,
    TRANSFER_DECLINED,
    TRANSFER_NO_SPACE,
    TRANSFER_NOT_RECEIVING,
    TRANSFER_TOO_MANY,
    TRANSFER_UNREADABLE,
    TRANSFER_FILE_CHANGED,
    TRANSFER_CORRUPT,
    TRANSFER_WRITE_FAILED,
    TRANSFER_FAILED,
    TRANSFER_ACCEPT,
    TRANSFER_DECLINE,
    TRANSFER_CANCEL,
    TRANSFER_CANCEL_NAMED,
    TRANSFER_SHOW,
    TRANSFER_SHOW_NAMED,
    TRANSFER_REMOVE,
    TRANSFER_REMOVE_NAMED,
    TRANSFER_FOLDER_FILES,
    TRANSFER_FOLDER_FILES_ONE,
    TRANSFER_FOLDER_PROGRESS,
    TRANSFER_SENT_SOME,
    TRANSFER_RECEIVED_SOME,
    TRANSFER_SHOW_FAILURES,
    TRANSFER_HIDE_FAILURES,
    TRANSFER_FAILURE_LINE,
    TRANSFER_FAILURES_MORE,
    TRANSFER_FOLDER,
    TRANSFER_SYMLINK,
    TRANSFER_NOT_A_FILE,
    TRANSFER_PEER_TOO_LARGE,
    TRANSFER_PEER_INVALID_NAME,
    TRANSFER_INVALID_NAME,
    TRANSFER_NO_SPACE_SHORT,
    CLIPBOARD_SENSITIVE,
    CLIPBOARD_TOO_LARGE,
    CLIPBOARD_SYNCED_FROM,
    CLIPBOARD_SENT_TO,
    CLIPBOARD_KIND_IMAGE,
    CLIPBOARD_KIND_RICH_TEXT,
    CLIPBOARD_KIND_TEXT,
    RELATIVE_JUST_NOW,
    RELATIVE_MINUTES_AGO,
    RELATIVE_MINUTES_ONE,
    RELATIVE_MINUTES_MANY,
    RELATIVE_HOURS_ONE,
    RELATIVE_HOURS_MANY,
    RELATIVE_DAYS_ONE,
    RELATIVE_DAYS_MANY,
    SEND_FILES_SEND,
    SEND_FILES_FAILED,
    SEND_FILES_CHECK_CONNECTED,
    CONNECTION_CONNECTED,
    CONNECTION_CONNECTED_RTT,
    CONNECTION_SCREEN_LOCKED,
    CONNECTION_SCREEN_LOCKED_RTT,
    CONNECTION_SCREEN_LOCKED_NO_INPUT,
    CONNECTION_SCREEN_LOCKED_NO_INPUT_RTT,
    CONNECTION_OFFLINE,
    CONNECTION_OFFLINE_AGO,
    CONNECTION_WAKING,
    CONNECTION_THIS_COMPUTER,
    OS_UNKNOWN,
    SCREEN_MAIN,
    SWAP_COMMAND_AND_CONTROL,
    REMOVE_DEVICE_TITLE,
    REMOVE_DEVICE_MESSAGE,
    REMOVE_DEVICE_CONFIRM,
    CANVAS_SYNCED,
    ADD_DEVICE_TITLE,
    PAIRING_PAIR,
    PAIRING_NOT_ACCEPTING,
    PAIRING_NONE_FOUND,
    PAIRING_NONE_FOUND_DETAIL,
    PAIRING_IP_ADDRESS,
    PAIRING_ADD_BY_IP,
    PAIRING_CONTINUE,
    PAIRING_START_OVER,
    PAIRING_CODE_NAME,
    PAIRING_CODE_TITLE,
    PAIRING_CODE_VALID,
    PAIRING_DECLINE,
    PAIRING_CONNECTING,
    PAIRING_ENTER_CODE,
    PAIRING_WRONG_CODE,
    PAIRING_PAIRED,
    PAIRING_THE_OTHER_COMPUTER,
    PAIRING_TOO_MANY_ATTEMPTS,
    PAIRING_EXPIRED,
    PAIRING_REJECTED,
    PAIRING_NOT_ALLOWED,
    PAIRING_BUSY,
    PAIRING_ID_CONFLICT,
    PAIRING_INCOMPATIBLE,
    PAIRING_UNREACHABLE,
    PAIRING_CANCELLED,
    PAIRING_DID_NOT_WORK,
    PAIRING_START_BUSY,
    PAIRING_START_INVALID_ADDRESS,
    PAIRING_START_UNREACHABLE,
    PAIRING_START_FAILED,
    NAV_ARRANGEMENT,
    NAV_DEVICES,
    NAV_FILES,
    NAV_GENERAL,
    NAV_TROUBLESHOOTING,
    MAIN_UNAVAILABLE,
    INSPECTOR_NAME,
    INSPECTOR_SYSTEM,
    INSPECTOR_STATUS,
    INSPECTOR_SCREENS,
    INSPECTOR_SCREENS_LATER,
    REMOVE_DEVICE,
    DEVICES_NONE_TITLE,
    DEVICES_NONE_DETAIL,
    FILES_RECEIVING,
    FILES_SAVE_IN,
    FILES_CHOOSE,
    FILES_WHEN_SENT,
    FILES_AUTOMATICALLY,
    FILES_ASK_EACH_TIME,
    FILES_SENDING,
    FILES_SEND_HINT,
    FILES_SEND_BUTTON,
    FILES_RECENT,
    FILES_NONE,
    FILES_CLEAR_HISTORY,
    FILES_CLEAR_TITLE,
    FILES_CLEAR_MESSAGE,
    FILES_CLEAR_CONFIRM,
    GENERAL_NAME,
    GENERAL_NAME_HINT,
    GENERAL_OPEN_AT_LOGIN,
    GENERAL_CLIPBOARD,
    GENERAL_CLIPBOARD_SYNC,
    GENERAL_CLIPBOARD_HINT,
    GENERAL_PICTURES,
    GENERAL_PICTURES_SHARE,
    GENERAL_PICTURES_HINT,
    GENERAL_PICTURES_UNAVAILABLE,
    GENERAL_SWITCHING,
    GENERAL_WAIT,
    GENERAL_WAIT_HINT,
    GENERAL_SHORTCUTS,
    SHORTCUT_TAKE_BACK,
    SHORTCUT_LOCK,
    SHORTCUT_JUMP,
    SHORTCUT_OFF,
    SHORTCUT_RESET,
    SHORTCUTS_HINT,
    SWITCH_MODIFIER_TITLE,
    SWITCH_MODIFIER_HINT,
    SWITCH_ALWAYS,
    SWITCH_HOLDING_CONTROL,
    SWITCH_HOLDING_ALT,
    SWITCH_HOLDING_SHIFT,
    IDENTIFY_SCREENS,
    IDENTIFY_SCREENS_HINT,
    CANVAS_RENAME,
    INSPECTOR_CALLS_ITSELF,
    INSPECTOR_SYNC_CLIPBOARD,
    INSPECTOR_SEND_FILES,
    INSPECTOR_SHARING_HINT,
    INSPECTOR_CURSOR_SPEED,
    INSPECTOR_CURSOR_SPEED_HINT,
    INSPECTOR_CURSOR_SPEED_RESET,
    WAKE,
    WAKE_NOT_WOKEN,
    WAKE_HOW_TO,
    WAKE_HOW_TO_URL,
    WAKE_NOT_TOLD_YET,
    DEVICE_FILES_OFF,
    DEVICE_FILES_OFF_SHORT,
    TROUBLE_PROBLEMS,
    TROUBLE_ALL_GOOD,
    TROUBLE_SERVICE,
    TROUBLE_VERSION,
    TROUBLE_BUILD,
    TROUBLE_KEYBOARD_MOUSE,
    TROUBLE_RESTART_HINT,
    TROUBLE_DEVICE_ID,
    TROUBLE_EXPORT,
    TROUBLE_EXPORT_HINT,
    NAME_INVALID,
    SERVICE_RUNNING,
    SERVICE_STARTING,
    SERVICE_UPDATING,
    SERVICE_STOPPING,
    SERVICE_STOPPED,
    SERVICE_NOT_INSTALLED,
    CAN_CONTROL,
    CANNOT_CONTROL_YET,
    DIAGNOSTICS_FILE_NAME,
    DIAGNOSTICS_FAILED,
    NOTIFY_RECEIVED_ONE,
    NOTIFY_RECEIVED_MANY,
    NOTIFY_OPEN_FOLDER,
    NOTIFY_OFFER_TITLE,
    NOTIFY_OFFER_BODY,
    NOTIFY_SERVICE_STOPPED,
    NOTIFY_LIST_SEPARATOR,
    NOTIFY_RECEIVED_ITEMS,
    NOTIFY_FAILED_ONE,
    NOTIFY_FAILED_MANY,
    ITEMS_FOLDER_ONE,
    ITEMS_FOLDER_MANY,
    ITEMS_FILE_ONE,
    ITEMS_FILE_MANY,
    ITEMS_AND,
    SEND_PICK_TITLE,
    SEND_SUMMARY_ONE_MORE,
    SEND_SUMMARY_MORE,
    SEND_REASON_FILES_OFF,
    SEND_NOWHERE,
    SEND_FILES_LIMITS,
    HUD_SENDING_ONE,
    HUD_SENDING_MANY,
    TITLE_RELATIVE,
    SUBTITLE_RELATIVE,
    GENERAL_RELATIVE_AUTO,
    GENERAL_RELATIVE_AUTO_HINT,
    GENERAL_RELATIVE_AUTO_HINT_NO_SHORTCUT,
    GENERAL_RELATIVE_AUTO_FAILED,
    SHORTCUT_RELATIVE,
    SWAP_SUPER_AND_CONTROL,
    SWITCH_HOLDING_SUPER,
    STATUS_HEADER,
    GENERAL_SHARING,
    ARRANGEMENT_DEVICES,
    ARRANGEMENT_PLACED_AT,
    ARRANGEMENT_NOT_PLACED,
    PAIRING_NEARBY,
    CLOSE,
    SERVICE_START_FAILED,
    SIZE_BYTE,
    SIZE_BYTES,
    FILES_CHOOSE_TITLE,
    FILES_SETTINGS_FAILED,
    SEND_FILES_CHOOSE_TITLE,
    SEND_FILES_CHOOSE_FOLDERS,
    SEND_FILES_FOLDERS,
    SHORTCUT_JUMP_OFF,
    SHORTCUT_RELATIVE_OFF,
    SHORTCUTS_RECORD_LATER,
    TRAY_NO_HOST_TITLE,
    DRAG_NOT_HERE,
    NAME_CHANGE_FAILED,
    COMMAND_FAILED,
    PAIRING_CODE_ENTRY,
    NOTIFY_OFFER_MANY,
    KEYBOARD_MOUSE_HERE,
    SCREEN_NUMBERED,
    PAIRING_SEND_AGAIN,
    SERVICE_RESTART_FAILED,
    CANVAS_DROP_TO_SEND,
    CANVAS_MOVE_CURSOR_HERE,
    CANVAS_LABEL,
    SHORTCUT_RECORD,
    SHORTCUT_PRESS_KEYS,
    SHORTCUT_PRESS_SINGLE,
    SHORTCUT_SINGLE_KEYS,
    SHORTCUT_DOUBLE_PRESS,
    SHORTCUT_DOUBLE_PRESS_HINT,
    SHORTCUT_PRESS_JUMP,
    SHORTCUT_NEEDS_MODIFIER,
    SHORTCUT_UNKNOWN_KEY,
    SHORTCUT_USED_FOR,
    SHORTCUT_USED_ELSEWHERE,
    SHORTCUT_NOT_CHANGED,
    SHORTCUT_NOT_SAVED,
    SHORTCUT_TURN_ON,
    MENU_RUN_SETUP,
    SETUP_WINDOW_TITLE,
    SETUP_STEP_OF,
    SETUP_BACK,
    SETUP_LATER,
    SETUP_CONTINUE,
    SETUP_DONE,
    SETUP_SKIP,
    SETUP_GET_STARTED,
    SETUP_WELCOME_TITLE,
    SETUP_BENEFIT_EDGES,
    SETUP_BENEFIT_CLIPBOARD,
    SETUP_BENEFIT_FILES,
    SETUP_REASSURANCE,
    SETUP_NAME_TITLE,
    SETUP_NAME_LABEL,
    SETUP_NAME_HINT,
    SETUP_PERMISSIONS_TITLE,
    SETUP_CONNECT_TITLE,
    SETUP_CONNECT_HINT,
    SETUP_CONNECT_BUTTON,
    SETUP_CONNECTED,
    SETUP_ARRANGE_TITLE,
    SETUP_ARRANGE_HINT,
    SETUP_ARRANGE_ALONE,
    SETUP_TRY_TITLE,
    SETUP_TRY_HINT,
    SETUP_TRY_DONE,
    SETUP_TRY_ALONE,
    HUD_TAKE_BACK_HINT,
    HUD_LOCKED_HERE,
    HUD_LOCKED_TO,
    HUD_UNLOCKED,
    HUD_PEER_LOST,
    HUD_REMOTE_ACTIVITY,
    HUD_BACK_HERE,
    HUD_LAYOUT_CHANGED,
    HUD_SCREEN_LOCKED,
    HUD_TWO_LINES,
    HUD_RELATIVE_ENTERED,
    HUD_RELATIVE_ENTERED_TAKE_BACK,
    HUD_RELATIVE_ENDED,
    HUD_RELATIVE_RELEASED,
    HUD_RELATIVE_UNSUPPORTED,
    CONNECTION_PEER_PAUSED,
    CONNECTION_PEER_PAUSED_RTT,
    CONNECTION_NO_INPUT,
    CONNECTION_NO_INPUT_RTT,
    HUD_PEER_PAUSED,
    HUD_CAPTURE_ENDED,
    HUD_INPUT_UNAVAILABLE,
    HUD_WAKING,
    HUD_AWAKE,
    SUBTITLE_CONTROLLED_WAYLAND,
    GENERAL_EDGE_GLOW,
    GENERAL_EDGE_GLOW_HINT,
    GENERAL_EDGE_GLOW_NONE,
    GENERAL_WAKE_ON_PUSH,
    GENERAL_WAKE_ON_PUSH_HINT,
    GENERAL_WAKE_ON_PUSH_EDGES_ONLY,
    EDGES_ONLY,
    TROUBLE_SESSION,
    TROUBLE_SESSION_HINT,
    TROUBLE_SESSION_TYPE,
    SESSION_UNKNOWN,
    ROLE_INPUT,
    ROLE_CAPTURE,
    ROLE_CLIPBOARD,
    ROLE_DISPLAYS,
    ROLE_DESKTOP_PICTURES,
    BACKEND_WITH_PROBLEM,
    BACKEND_NONE,
    CLIPBOARD_NEEDS_REMOTE_CONTROL,
    TROUBLE_FIREWALL,
    FIREWALL_BLOCKS_PORT,
    FIREWALL_BLOCKS_MDNS,
    FIREWALL_BLOCKS_BOTH,
    FIREWALL_UNREADABLE,
    FIREWALL_COMMAND,
    COPY,
    COPIED,
    TROUBLE_PERMISSIONS,
    PERMISSIONS_HINT,
    PERMISSION_REMOTE_CONTROL,
    PERMISSION_INPUT_CAPTURE,
    PERMISSION_GRANTED,
    PERMISSION_NOT_ASKED,
    PERMISSION_ASKING,
    PERMISSION_DENIED,
    PERMISSION_FAILED,
    PERMISSION_FAILED_PLAIN,
    PERMISSION_RESTORING,
    PERMISSION_STOPPED,
    PERMISSION_EVERY_LOGIN,
    PERMISSION_ALLOW,
    PERMISSION_ALLOW_AGAIN,
    TROUBLE_JOURNAL,
    TROUBLE_JOURNAL_HINT,
    TROUBLE_JOURNAL_APP,
    GENERAL_EDGE_GLOW_UNAVAILABLE,
    TITLE_APP_OLDER,
    APP_OLDER_HINT,
    LICENSE_NAV,
    LICENSE_TRIAL,
    LICENSE_LICENSED,
    LICENSE_TRIAL_BODY,
    LICENSE_FULL_BODY,
    LICENSE_INVALID_BODY,
    LICENSE_UNAVAILABLE_BODY,
    LICENSE_PRICE,
    LICENSE_BUY,
    LICENSE_ENTER,
    LICENSE_MANAGE,
    LICENSE_TRY_AGAIN,
    LICENSE_HOW_TITLE,
    LICENSE_HOW_BUY,
    LICENSE_HOW_ENTER,
    LICENSE_HOW_PAIRED,
    LICENSE_POLAR_NOTE,
    LICENSE_LICENSED_TO,
    LICENSE_THANKS,
    LICENSE_FROM_PAIRED,
    LICENSE_COMPUTERS,
    LICENSE_COUNT,
    LICENSE_THIS_COMPUTER,
    LICENSE_PAIRED,
    LICENSE_LIST_NOTE,
    LICENSE_REMOVE,
    LICENSE_ENTER_TITLE,
    LICENSE_KEY_LABEL,
    LICENSE_KEY_HELP,
    LICENSE_ACTIVATE,
    LICENSE_ACTIVATING,
    LICENSE_ERR_INVALID,
    LICENSE_ERR_FULL,
    LICENSE_ERR_OFFLINE,
    LICENSE_REMOVE_TITLE,
    LICENSE_REMOVE_BODY,
    LICENSE_REMOVE_CONFIRM,
    LICENSE_REMOVED_OFFLINE,
    MENU_ABOUT,
    UPDATES_TITLE,
    ABOUT_GPL,
    LICENSE_ERR_RECORD,
    LICENSE_REMOVING,
    LICENSE_REMOVE_FAILED,
    LICENSE_ERR_UNAVAILABLE,
    UPDATES_VERSION,
    UPDATES_BODY,
    UPDATES_SOURCE,
    UPDATES_NOTE,
    CONNECTION_OFFLINE_FOR_NOW,
    OFFLINE_SET_BY,
    OFFLINE_SET_HERE,
    OFFLINE_UNAWARE,
    OFFLINE_TAKE,
    OFFLINE_BRING_BACK,
    OFFLINE_EXPLAIN,
    MENU_TAKE_THIS_OFFLINE,
    TITLE_THIS_OFFLINE,
    SUBTITLE_THIS_OFFLINE_BY,
    SUBTITLE_THIS_OFFLINE_HERE,
    TRAY_THIS_OFFLINE,
    HUD_OFFLINE,
    HUD_BACK_ONLINE,
    HUD_OFFLINE_RETURNED,
    HUD_TOOK_THIS_OFFLINE,
    OFFLINE_SET_BY_UNKNOWN,
    SUBTITLE_THIS_OFFLINE_BY_UNKNOWN,
    HUD_TOOK_THIS_OFFLINE_UNKNOWN,
    NAV_PROFILES,
    PROFILE_IN_USE,
    PROFILE_IN_USE_HINT,
    PROFILE_AUTOMATIC,
    PROFILE_AUTOMATIC_NOW,
    PROFILE_DEFAULT,
    PROFILE_OTHER_NETWORKS,
    PROFILE_NEW_NAME,
    PROFILE_CURRENT_NETWORK,
    PROFILE_NO_NETWORK,
    NETWORK_ROUTER,
    NETWORK_VPN,
    NETWORK_WIRED,
    NETWORK_WIFI,
    PROFILE_NAME_TAKEN,
    PROFILE_ADD_TO,
    PROFILE_IN_USE_TAG,
    PROFILE_ADD,
    PROFILE_REMOVE,
    PROFILE_SHARING,
    PROFILE_SHARE,
    PROFILE_PAUSE,
    PROFILE_PAUSED_HINT,
    PROFILE_COMPUTERS,
    PROFILE_COMPUTERS_HINT,
    PROFILE_FILES,
    PROFILE_CLIPBOARD_HINT,
    PROFILE_NETWORKS,
    PROFILE_NO_NETWORKS,
    PROFILE_OTHER_NETWORKS_HINT,
    PROFILE_ADD_CURRENT_NETWORK,
    PROFILE_REMOVE_NETWORK,
    MENU_PROFILE,
    MENU_MANAGE_PROFILES,
    TITLE_PROFILE_PAUSED,
    PROFILE_NETWORK_NOT_IN_ANY,
    PROFILE_ADD_THIS_NETWORK,
    CONNECTION_NOT_IN_PROFILE,
    HUD_PROFILE_SWITCHED,
    HUD_PROFILE_SWITCHED_PAUSED,
];
