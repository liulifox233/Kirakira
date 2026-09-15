//! windowEx.dll — wtnbgo's Win32 window extension for the engine's shell
//! objects, ported member by member.
//!
//! # Reference
//!
//! Two copies of the plugin exist in the reference trees:
//!
//! * `krkrz/src/plugins/win32/windowEx/main.cpp` (2 094 lines) is a frozen
//!   2013 import (`f74bf363`, "吉里吉里2プラグインをそのまま追加").
//! * `krkr2/.../win32/windowEx/main.cpp` (2 164 lines) kept being revised;
//!   its last change is 2016-04-03 and it is the **superset**: it adds
//!   `maximizeBox`, `minimizeBox`, `disableMove`, `resetExSystemMenu`, the
//!   `WM_QUERYOPEN` event and switches `_loadExternalIcon` to
//!   `TVPGetLocallyAccessibleName` (`main.cpp:100-111`).
//!
//! This port follows the krkr2 copy; a citation without a tree prefix is a
//! line of that `main.cpp`, while engine-internal files are cited from the
//! krkrz tree (`krkrz/src/core/…`) that this engine's own comments use. All
//! sources are Shift-JIS.
//!
//! # What the reference is
//!
//! NCB (`ncbind.hpp`) binds six surfaces onto objects that already exist:
//! `Window` and `MenuItem` (attach-class), `Pad` (attach-class),
//! `Debug.console` (attach-function), `System` and `Scripts`
//! (attach-function). `WindowEx` also installs a native message receiver
//! (`main.cpp:617-630`) so that the Win32 message procedure's non-client
//! events reach TJS as `onMinimize`…`onWindowsMessageHook`
//! (`main.cpp:8-32`).
//!
//! # What this port covers
//!
//! Working, with the reference's names, argument contracts and error
//! behaviour:
//!
//! * the 24 `Window.ncht*` and 10 `MenuItem.bi*` constants;
//! * `Window._Notifications` — the lazily built `WM_*` name/number table
//!   (`main.cpp:726-805`) and the two lookups `getNotificationNum` /
//!   `getNotificationName` (`main.cpp:695-708`);
//! * `Window.registerExEvent()`'s one deferred member check per event
//!   (`main.cpp:662-667`);
//! * the per-instance state `disableResize`, `disableMove`,
//!   `enableNCMouseEvent` and `exSystemMenu` (`main.cpp:255-330`);
//! * `Window.setMessageHook`'s name/number resolution and its
//!   `0..0x3FF` rejection, including the returned "hook enabled" flag
//!   (`main.cpp:341-362`, `:675-693`);
//! * `System.readEnvValue` / `System.expandEnvString` (`main.cpp:1977-2015`);
//! * `Scripts.setEvalErrorLog`'s "return the previous value" contract
//!   (`main.cpp:2111-2115`);
//! * `System.getSystemMetrics`' validation and its `System.metrics` table;
//!   the six values the engine publishes (`CXSCREEN`, `CYSCREEN`,
//!   `X/YVIRTUALSCREEN`, `CX/CYVIRTUALSCREEN`) come from the engine, the rest
//!   answer 0 while the host has no metrics source (`main.cpp:1919-1974`);
//! * `Window.getWindowRect` / `getClientRect` / `getNormalRect` from the
//!   engine's window model, which stores the same quantities (krkrz
//!   `src/core/visual/WindowIntf.cpp` `:1317` width, `:1437` left, `:1457`
//!   top, `:1559` innerWidth; the values come from
//!   `src/core/environ/win32/TVPWindow.cpp:636-761`. krkr2 trunk keeps this
//!   geometry in `Win32/WindowImpl.cpp` instead, which is why the two trees'
//!   line numbers differ);
//! * `Window.ncHitTest`'s argument count check and the reference's own
//!   null-handle answer: nothing in this engine writes `Window.HWND`, so
//!   `SendMessage` reaches no window and answers 0, never an `HT*` code
//!   (`main.cpp:332-339`);
//! * `MenuItem.rightJustify` / `bmpItem` / `bmpChecked` / `bmpUnchecked`
//!   including the `no layer object.` error for a non-`Layer` value
//!   (`main.cpp:1116-1170`).
//!
//! # Engine capabilities the rest needs
//!
//! These members exist and validate their arguments the way the reference
//! does, then take the reference's own "no window handle" path and report the
//! gap once through the host log:
//!
//! * **desktop window operations** — `minimize`/`maximize`/`showRestore`/
//!   `focusMenuByKey`/`bringTo`/`sendToBack`/`resetWindowIcon`/
//!   `setWindowIcon`/`resetExSystemMenu`/`setOverlayBitmap` and the
//!   `maximizeBox`/`minimizeBox`/`maximized`/`minimized` state: the reference
//!   `PostMessage`s `SC_*` commands and reads `GetWindowLong`/
//!   `IsZoomed`/`IsIconic`/`GetWindowPlacement` from a real `HWND`
//!   (`main.cpp:76-405`);
//! * **window/application icons** — `setWindowIcon`/`setApplicationIcon`
//!   resolve their storage argument here (throwing the reference's
//!   `file not found.` / `cannot get in archive icon.`), but loading an
//!   `.ico` and posting `WM_SETICON` needs a platform icon loader;
//! * **monitors and cursor** — `System.getDisplayMonitors` /
//!   `getMonitorInfo` / `getCursorPos` / `setCursorPos` (`main.cpp:1826-1918`);
//! * **host metrics** — `System.getSystemMetrics` for every metric the engine
//!   does not publish, and `System.getDoubleClickTime`;
//! * **a window message channel** — the `on*` extended events
//!   (`WINDOW_EX_EVENTS` below), `Window.setMessageHook`'s hooks and
//!   `Pad.registerExEvent`'s `onClose`;
//! * **Win32 menu backing** — `MenuItem.popupEx`, and the menu-bar update the
//!   `MenuItem` icon properties perform after storing their value;
//! * **a `Pad` class** (krkr2 `src/core/utils/PadIntf.cpp`) and a
//!   `Debug.console` object (console.dll): `NCB_ATTACH_CLASS_WITH_HOOK`
//!   attaches to the existing class only, which is what
//!   `NCB_ATTACH_FUNCTION` already does for a missing `Debug.console`
//!   (`ncbind.hpp:2324-2345` returns without registering);
//! * **an engine expression evaluator** — the reference replaces
//!   `Scripts.eval` and routes it through `TVPExecuteExpression` when
//!   `setEvalErrorLog(false)` is in effect (`main.cpp:2118-2129`).
//!
//! None of them can be backed from this module alone: every one needs
//! `crates/krkr-engine` to grow a host-facing piece, and the shapes below are
//! the smallest ones that make the member real. They are not a theoretical
//! surface either — PARQUET's own `system/MainWindow.tjs` calls
//! `System.getCursorPos`, `getDisplayMonitors` and `getMonitorInfo` (and its
//! `system/Menus.tjs` / `main.xp3:uisystem.tjs` call `maximize`, `minimize`
//! and `showRestore`), so the gaps are live for shipped content:
//!
//! * **cursor** — widen `KrkrHost::cursor_position` / `set_cursor_position`
//!   (`host.rs:1416-1422`, `pub(crate)` today) to `pub`. The point the engine
//!   stores is the frame's client-space `CursorMoved` position
//!   (`engine.rs:1978`, `:2122`), so the exposed view has to be in desktop
//!   coordinates like the reference's `GetCursorPos` — the shell can add the
//!   main window's `left`/`top` (`host.rs:1976`), which it already keeps in
//!   desktop coordinates. `System.getCursorPos` then answers the `%[x, y]`
//!   dictionary (`main.cpp:1896-1908`) and `setCursorPos` its boolean, with
//!   `false` — the reference's failed `SetCursorPos` — wherever the host
//!   cannot move the OS cursor.
//! * **monitors** — a `pub fn monitors(&self) -> Vec<MonitorSnapshot>`
//!   (`name`, `primary`, monitor rect, work rect) fed from the shell's
//!   `winit::monitor` list; `System.getDisplayMonitors` / `getMonitorInfo`
//!   map onto it (`main.cpp:1782-1893`).
//! * **desktop window operations** — a `pub fn window_action(&mut self,
//!   window: ObjectHandle, action: WindowAction) -> bool` the shell drains,
//!   the pattern `take_external_resource_requests` (`host.rs:1273`) already
//!   uses, plus `pub fn window_state(&self, window: ObjectHandle) ->
//!   WindowState` for `maximized`/`minimized` and the two box flags. Their
//!   initial values come from the window style the reference creates —
//!   `WS_OVERLAPPEDWINDOW`, i.e. `WS_MAXIMIZEBOX`/`WS_MINIMIZEBOX` on — which
//!   is also why a window-less port answering 0 is a value the reference would
//!   not produce for a live game window.
//! * **icons** — the same action channel carrying a decoded icon; the `.ico`
//!   decoder can live on either side, the window update cannot.
//! * **message channel** — a `pub fn push_window_event(&mut self,
//!   WindowEvent) -> Result<()>` the shell calls from its window-event loop,
//!   dispatching the names tabulated in [`WINDOW_EX_EVENTS`] and reaching
//!   `setMessageHook`'s bit hooks.
//! * **`Pad` class** — a `Pad` native class in `native/classes.rs` (krkr2
//!   `src/core/utils/PadIntf.cpp`), installed through `install_native_class`
//!   (`classes.rs:43`); `install_pad_ex` above attaches `registerExEvent` as
//!   soon as the global exists.
//! * **`Debug.console`** — the object belongs to console.dll, not to windowEx:
//!   neither shipped game ships console.dll, so the reference has no
//!   `Debug.console` for this content either, and backing its seven functions
//!   is a console-window emulation rather than a windowEx change.
//!
//! Nothing here is invented: a member the engine cannot back either keeps the
//! reference's value for a window-less object or is absent with the
//! capability named in [`META`]. The few places where this port answers a
//! *value* the reference would not produce for such an object — the rect
//! getters, the engine's published screen metrics, `getAboutString` and
//! `getDoubleClickTime` — are listed under "Deliberate deviations" below.
//!
//! # Deliberate deviations
//!
//! Where the reference's behaviour depends on a facility this engine does not
//! have, the port keeps the reference's *shape* and says what it skipped
//! rather than inventing an effect:
//!
//! * `MenuItem`'s icon properties store their value and report the missing
//!   menu-bar update; the reference throws `Cannot get parent menu.` from
//!   `updateMenuItemInfo` (`main.cpp:1188-1197`), which every item would hit
//!   here because no engine-side object has a parent `HMENU`.
//! * `getWindowRect` / `getClientRect` / `getNormalRect` read the engine's
//!   window model (`Window.left/top/width/height`, `innerWidth`/`innerHeight`)
//!   — the same quantities the official getters read (krkrz
//!   `src/core/environ/win32/TVPWindow.cpp:636-761`) — instead of calling
//!   `GetWindowRect` on a handle.
//! * `getSystemMetrics` answers the geometry the engine publishes on `System`
//!   (`screenWidth`, `screenHeight`, `desktopLeft/Top/Width/Height`) for the
//!   six metrics that name it, and 0 for the rest.
//! * `getDoubleClickTime` answers the documented default of 500 ms
//!   (`manual.tjs:519-525`) because there is no host setting to read.
//! * `getAboutString` answers this engine's own identity: the reference renders
//!   a version resource plus the important log (`MsgIntf.cpp:163-177`).
//! * Per-object state — the three `disable*` flags, `exSystemMenu`, the
//!   message-hook bits and the `registerExEvent` caches — lives in one hidden
//!   member of the object, because a TJS object here has no native instance to
//!   carry it the way `WindowEx` does (`main.cpp:807-818`).
//! * `Scripts.setEvalErrorLog`'s flag is stored per `Scripts` object instead of
//!   in the process-wide static (`main.cpp:2143`). It is only observable
//!   through the setter's return value, and a per-runtime home keeps two
//!   engines inside one process independent.

use std::{
    collections::BTreeSet,
    sync::Mutex,
};

use krkr_engine::{KrkrHost, KrkrPlugin};
use krkr_tjs2::{
    Result, TjsError,
    runtime::{NativeArgCount, ObjectHandle, Runtime, Variant},
};

use crate::catalog::{PluginMeta, PluginStatus};

pub(crate) const META: PluginMeta = PluginMeta {
    status: PluginStatus::Shim,
    feature: "Window/MenuItem/Pad/Debug.console/System/Scripts extensions",
    notes: "Surface, constants and argument/error behaviour ported from windowEx/main.cpp \
            (krkr2 trunk copy, the newer superset; krkrz ships a 2013 snapshot without \
            maximizeBox/minimizeBox/disableMove/resetExSystemMenu). Real: ncht* and bi* constants, \
            Window._Notifications + getNotificationNum/getNotificationName, registerExEvent's \
            event caches, disableResize/disableMove/enableNCMouseEvent/exSystemMenu state, \
            setMessageHook's 0..0x3FF check and bit bookkeeping, ncHitTest's count check and the \
            reference's null-handle answer (0: nothing in this engine writes Window.HWND), \
            readEnvValue/expandEnvString, Scripts.setEvalErrorLog, getSystemMetrics' metric table, \
            the window-rect getters from the engine's window model, MenuItem icon properties with \
            the reference's no-layer-object error, and the icon members' storage lookup \
            (file not found. / cannot get in archive icon. for a name without a locally \
            accessible form). Members that still need engine support \
            (desktop window ops incl. maximize/minimize/restore, z-order, icons and the overlay \
            bitmap, host monitor/cursor/metrics sources, a window message channel for the on* \
            events, Win32 menu backing for popupEx and the MenuItem menu-bar update, a Pad class, \
            a Debug.console object, an expression evaluator for the Scripts.eval override) \
            validate like the reference and then take its no-window-handle path, reported once \
            through the host log.",
    install: |engine| engine.register_plugin(WindowExPlugin),
};

pub struct WindowExPlugin;

impl KrkrPlugin for WindowExPlugin {
    fn name(&self) -> &str {
        "windowEx.dll"
    }

    fn register(&self, runtime: &mut Runtime<KrkrHost>) -> Result<()> {
        install_window_ex(runtime);
        install_menu_item_ex(runtime);
        install_pad_ex(runtime);
        install_console_ex(runtime);
        install_system_ex(runtime);
        install_scripts_ex(runtime);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// `Window`'s non-client hit-test constants, `main.cpp:986-1009`. Each is the
/// Win32 `HT*` value masked to 16 bits (`HTERROR` and `HTTRANSPARENT` are
/// negative), which is why `nchtError`/`nchtTransparent` are 65534/65535.
const WINDOW_HIT_TEST_CONSTANTS: &[(&str, i64)] = &[
    ("nchtError", 0xFFFE),
    ("nchtTransparent", 0xFFFF),
    ("nchtNoWhere", 0),
    ("nchtClient", 1),
    ("nchtCaption", 2),
    ("nchtSysMenu", 3),
    ("nchtSize", 4),
    ("nchtGrowBox", 4),
    ("nchtMenu", 5),
    ("nchtHScroll", 6),
    ("nchtVScroll", 7),
    ("nchtMinButton", 8),
    ("nchtReduce", 8),
    ("nchtMaxButton", 9),
    ("nchtZoom", 9),
    ("nchtLeft", 10),
    ("nchtRight", 11),
    ("nchtTop", 12),
    ("nchtTopLeft", 13),
    ("nchtTopRight", 14),
    ("nchtBottom", 15),
    ("nchtBottomLeft", 16),
    ("nchtBottomRight", 17),
    ("nchtBorder", 18),
];

/// `MenuItem`'s bitmap constants, `main.cpp:1422-1431`: the Win32 `HBMMENU_*`
/// handle values.
const MENU_ITEM_BITMAP_CONSTANTS: &[(&str, i64)] = &[
    ("biSystem", 1),
    ("biRestore", 2),
    ("biMinimize", 3),
    ("biClose", 5),
    ("biCloseDisabled", 6),
    ("biMinimizeDisabled", 7),
    ("biPopupClose", 8),
    ("biPopupRestore", 9),
    ("biPopupMaximize", 10),
    ("biPopupMinimize", 11),
];

/// `Window._Notifications`, built on first use by `main.cpp:726-805`: every
/// `WM_*` message name the reference exposes *without* the `WM_` prefix (the
/// `WM(key)` macro stringifies its argument), mapped to its numeric value,
/// plus the reverse mapping used by `getNotificationName`.
///
/// The order is the reference's macro order because the table is inserted
/// twice per entry and later duplicates win the reverse mapping: `WININICHANGE`
/// and `SETTINGCHANGE` are both `0x001A`, so `getNotificationName(0x001A)`
/// answers `"SETTINGCHANGE"` exactly like the reference dictionary does.
const WINDOW_NOTIFICATIONS: &[(&str, i64)] = &[
    ("NULL", 0x0000), ("CREATE", 0x0001), ("DESTROY", 0x0002), ("MOVE", 0x0003),
    ("SIZE", 0x0005), ("ACTIVATE", 0x0006), ("SETFOCUS", 0x0007), ("KILLFOCUS", 0x0008),
    ("ENABLE", 0x000A), ("SETREDRAW", 0x000B), ("SETTEXT", 0x000C), ("GETTEXT", 0x000D),
    ("GETTEXTLENGTH", 0x000E), ("PAINT", 0x000F), ("CLOSE", 0x0010), ("QUERYENDSESSION", 0x0011),
    ("QUERYOPEN", 0x0013), ("ENDSESSION", 0x0016), ("QUIT", 0x0012), ("ERASEBKGND", 0x0014),
    ("SYSCOLORCHANGE", 0x0015), ("SHOWWINDOW", 0x0018), ("CTLCOLOR", 0x0019), ("WININICHANGE", 0x001A),
    ("SETTINGCHANGE", 0x001A), ("DEVMODECHANGE", 0x001B), ("ACTIVATEAPP", 0x001C), ("FONTCHANGE", 0x001D),
    ("TIMECHANGE", 0x001E), ("CANCELMODE", 0x001F), ("SETCURSOR", 0x0020), ("MOUSEACTIVATE", 0x0021),
    ("CHILDACTIVATE", 0x0022), ("QUEUESYNC", 0x0023), ("GETMINMAXINFO", 0x0024), ("PAINTICON", 0x0026),
    ("ICONERASEBKGND", 0x0027), ("NEXTDLGCTL", 0x0028), ("SPOOLERSTATUS", 0x002A), ("DRAWITEM", 0x002B),
    ("MEASUREITEM", 0x002C), ("DELETEITEM", 0x002D), ("VKEYTOITEM", 0x002E), ("CHARTOITEM", 0x002F),
    ("SETFONT", 0x0030), ("GETFONT", 0x0031), ("SETHOTKEY", 0x0032), ("GETHOTKEY", 0x0033),
    ("QUERYDRAGICON", 0x0037), ("COMPAREITEM", 0x0039), ("GETOBJECT", 0x003D), ("COMPACTING", 0x0041),
    ("COMMNOTIFY", 0x0044), ("WINDOWPOSCHANGING", 0x0046), ("WINDOWPOSCHANGED", 0x0047), ("POWER", 0x0048),
    ("COPYDATA", 0x004A), ("CANCELJOURNAL", 0x004B), ("NOTIFY", 0x004E), ("INPUTLANGCHANGEREQUEST", 0x0050),
    ("INPUTLANGCHANGE", 0x0051), ("TCARD", 0x0052), ("HELP", 0x0053), ("USERCHANGED", 0x0054),
    ("NOTIFYFORMAT", 0x0055), ("CONTEXTMENU", 0x007B), ("STYLECHANGING", 0x007C), ("STYLECHANGED", 0x007D),
    ("DISPLAYCHANGE", 0x007E), ("GETICON", 0x007F), ("SETICON", 0x0080), ("NCCREATE", 0x0081),
    ("NCDESTROY", 0x0082), ("NCCALCSIZE", 0x0083), ("NCHITTEST", 0x0084), ("NCPAINT", 0x0085),
    ("NCACTIVATE", 0x0086), ("GETDLGCODE", 0x0087), ("SYNCPAINT", 0x0088), ("NCMOUSEMOVE", 0x00A0),
    ("NCLBUTTONDOWN", 0x00A1), ("NCLBUTTONUP", 0x00A2), ("NCLBUTTONDBLCLK", 0x00A3), ("NCRBUTTONDOWN", 0x00A4),
    ("NCRBUTTONUP", 0x00A5), ("NCRBUTTONDBLCLK", 0x00A6), ("NCMBUTTONDOWN", 0x00A7), ("NCMBUTTONUP", 0x00A8),
    ("NCMBUTTONDBLCLK", 0x00A9), ("NCXBUTTONDOWN", 0x00AB), ("NCXBUTTONUP", 0x00AC), ("NCXBUTTONDBLCLK", 0x00AD),
    ("INPUT_DEVICE_CHANGE", 0x00FE), ("INPUT", 0x00FF), ("KEYFIRST", 0x0100), ("KEYDOWN", 0x0100),
    ("KEYUP", 0x0101), ("CHAR", 0x0102), ("DEADCHAR", 0x0103), ("SYSKEYDOWN", 0x0104),
    ("SYSKEYUP", 0x0105), ("SYSCHAR", 0x0106), ("SYSDEADCHAR", 0x0107), ("UNICHAR", 0x0109),
    ("KEYLAST", 0x0109), ("IME_STARTCOMPOSITION", 0x010D), ("IME_ENDCOMPOSITION", 0x010E), ("IME_COMPOSITION", 0x010F),
    ("IME_KEYLAST", 0x010F), ("INITDIALOG", 0x0110), ("COMMAND", 0x0111), ("SYSCOMMAND", 0x0112),
    ("TIMER", 0x0113), ("HSCROLL", 0x0114), ("VSCROLL", 0x0115), ("INITMENU", 0x0116),
    ("INITMENUPOPUP", 0x0117), ("MENUSELECT", 0x011F), ("MENUCHAR", 0x0120), ("ENTERIDLE", 0x0121),
    ("MENURBUTTONUP", 0x0122), ("MENUDRAG", 0x0123), ("MENUGETOBJECT", 0x0124), ("UNINITMENUPOPUP", 0x0125),
    ("MENUCOMMAND", 0x0126), ("CHANGEUISTATE", 0x0127), ("UPDATEUISTATE", 0x0128), ("QUERYUISTATE", 0x0129),
    ("CTLCOLORMSGBOX", 0x0132), ("CTLCOLOREDIT", 0x0133), ("CTLCOLORLISTBOX", 0x0134), ("CTLCOLORBTN", 0x0135),
    ("CTLCOLORDLG", 0x0136), ("CTLCOLORSCROLLBAR", 0x0137), ("CTLCOLORSTATIC", 0x0138), ("MOUSEFIRST", 0x0200),
    ("MOUSEMOVE", 0x0200), ("LBUTTONDOWN", 0x0201), ("LBUTTONUP", 0x0202), ("LBUTTONDBLCLK", 0x0203),
    ("RBUTTONDOWN", 0x0204), ("RBUTTONUP", 0x0205), ("RBUTTONDBLCLK", 0x0206), ("MBUTTONDOWN", 0x0207),
    ("MBUTTONUP", 0x0208), ("MBUTTONDBLCLK", 0x0209), ("MOUSEWHEEL", 0x020A), ("XBUTTONDOWN", 0x020B),
    ("XBUTTONUP", 0x020C), ("XBUTTONDBLCLK", 0x020D), ("MOUSEHWHEEL", 0x020E), ("MOUSELAST", 0x020E),
    ("PARENTNOTIFY", 0x0210), ("ENTERMENULOOP", 0x0211), ("EXITMENULOOP", 0x0212), ("NEXTMENU", 0x0213),
    ("SIZING", 0x0214), ("CAPTURECHANGED", 0x0215), ("MOVING", 0x0216), ("POWERBROADCAST", 0x0218),
    ("DEVICECHANGE", 0x0219), ("MDICREATE", 0x0220), ("MDIDESTROY", 0x0221), ("MDIACTIVATE", 0x0222),
    ("MDIRESTORE", 0x0223), ("MDINEXT", 0x0224), ("MDIMAXIMIZE", 0x0225), ("MDITILE", 0x0226),
    ("MDICASCADE", 0x0227), ("MDIICONARRANGE", 0x0228), ("MDIGETACTIVE", 0x0229), ("MDISETMENU", 0x0230),
    ("ENTERSIZEMOVE", 0x0231), ("EXITSIZEMOVE", 0x0232), ("DROPFILES", 0x0233), ("MDIREFRESHMENU", 0x0234),
    ("IME_SETCONTEXT", 0x0281), ("IME_NOTIFY", 0x0282), ("IME_CONTROL", 0x0283), ("IME_COMPOSITIONFULL", 0x0284),
    ("IME_SELECT", 0x0285), ("IME_CHAR", 0x0286), ("IME_REQUEST", 0x0288), ("IME_KEYDOWN", 0x0290),
    ("IME_KEYUP", 0x0291), ("MOUSEHOVER", 0x02A1), ("MOUSELEAVE", 0x02A3), ("NCMOUSEHOVER", 0x02A0),
    ("NCMOUSELEAVE", 0x02A2), ("WTSSESSION_CHANGE", 0x02B1), ("TABLET_FIRST", 0x02C0), ("TABLET_LAST", 0x02DF),
    ("CUT", 0x0300), ("COPY", 0x0301), ("PASTE", 0x0302), ("CLEAR", 0x0303),
    ("UNDO", 0x0304), ("RENDERFORMAT", 0x0305), ("RENDERALLFORMATS", 0x0306), ("DESTROYCLIPBOARD", 0x0307),
    ("DRAWCLIPBOARD", 0x0308), ("PAINTCLIPBOARD", 0x0309), ("VSCROLLCLIPBOARD", 0x030A), ("SIZECLIPBOARD", 0x030B),
    ("ASKCBFORMATNAME", 0x030C), ("CHANGECBCHAIN", 0x030D), ("HSCROLLCLIPBOARD", 0x030E), ("QUERYNEWPALETTE", 0x030F),
    ("PALETTEISCHANGING", 0x0310), ("PALETTECHANGED", 0x0311), ("HOTKEY", 0x0312), ("PRINT", 0x0317),
    ("PRINTCLIENT", 0x0318), ("APPCOMMAND", 0x0319), ("THEMECHANGED", 0x031A), ("CLIPBOARDUPDATE", 0x031D),
    ("DWMCOMPOSITIONCHANGED", 0x031E), ("DWMNCRENDERINGCHANGED", 0x031F), ("DWMCOLORIZATIONCOLORCHANGED", 0x0320), ("DWMWINDOWMAXIMIZEDCHANGE", 0x0321),
    ("GETTITLEBARINFOEX", 0x033F), ("HANDHELDFIRST", 0x0358), ("HANDHELDLAST", 0x035F), ("AFXFIRST", 0x0360),
    ("AFXLAST", 0x037F), ("PENWINFIRST", 0x0380), ("PENWINLAST", 0x038F),
];

/// `System.metrics`, built on first use by `main.cpp:1936-1968`: the `SM_*`
/// names without their prefix, upper-case, mapped to the Win32 index
/// `GetSystemMetrics` takes. `getSystemMetrics` upper-cases the name it is
/// given and looks it up here.
const SYSTEM_METRICS: &[(&str, i64)] = &[
    ("ARRANGE", 56), ("CLEANBOOT", 67), ("CMONITORS", 80), ("CMOUSEBUTTONS", 43),
    ("CXBORDER", 5), ("CXCURSOR", 13), ("CXDLGFRAME", 7), ("CXDOUBLECLK", 36),
    ("CXDRAG", 68), ("CXEDGE", 45), ("CXFIXEDFRAME", 7), ("CXFOCUSBORDER", 83),
    ("CXFRAME", 32), ("CXFULLSCREEN", 16), ("CXHSCROLL", 21), ("CXHTHUMB", 10),
    ("CXICON", 11), ("CXICONSPACING", 38), ("CXMAXIMIZED", 61), ("CXMAXTRACK", 59),
    ("CXMENUCHECK", 71), ("CXMENUSIZE", 54), ("CXMIN", 28), ("CXMINIMIZED", 57),
    ("CXMINSPACING", 47), ("CXMINTRACK", 34), ("CXPADDEDBORDER", 92), ("CXSCREEN", 0),
    ("CXSIZE", 30), ("CXSIZEFRAME", 32), ("CXSMICON", 49), ("CXSMSIZE", 52),
    ("CXVIRTUALSCREEN", 78), ("CXVSCROLL", 2), ("CYBORDER", 6), ("CYCAPTION", 4),
    ("CYCURSOR", 14), ("CYDLGFRAME", 8), ("CYDOUBLECLK", 37), ("CYDRAG", 69),
    ("CYEDGE", 46), ("CYFIXEDFRAME", 8), ("CYFOCUSBORDER", 84), ("CYFRAME", 33),
    ("CYFULLSCREEN", 17), ("CYHSCROLL", 3), ("CYICON", 12), ("CYICONSPACING", 39),
    ("CYKANJIWINDOW", 18), ("CYMAXIMIZED", 62), ("CYMAXTRACK", 60), ("CYMENU", 15),
    ("CYMENUCHECK", 72), ("CYMENUSIZE", 55), ("CYMIN", 29), ("CYMINIMIZED", 58),
    ("CYMINSPACING", 48), ("CYMINTRACK", 35), ("CYSCREEN", 1), ("CYSIZE", 31),
    ("CYSIZEFRAME", 33), ("CYSMCAPTION", 51), ("CYSMICON", 50), ("CYSMSIZE", 53),
    ("CYVIRTUALSCREEN", 79), ("CYVSCROLL", 20), ("CYVTHUMB", 9), ("DBCSENABLED", 42),
    ("DEBUG", 22), ("IMMENABLED", 82), ("MEDIACENTER", 87), ("MENUDROPALIGNMENT", 40),
    ("MIDEASTENABLED", 74), ("MOUSEPRESENT", 19), ("MOUSEHORIZONTALWHEELPRESENT", 91), ("MOUSEWHEELPRESENT", 75),
    ("NETWORK", 63), ("PENWINDOWS", 41), ("REMOTECONTROL", 0x2001), ("REMOTESESSION", 0x1000),
    ("SAMEDISPLAYFORMAT", 81), ("SECURE", 44), ("SERVERR2", 89), ("SHOWSOUNDS", 70),
    ("SHUTTINGDOWN", 0x2000), ("SLOWMACHINE", 73), ("STARTER", 88), ("SWAPBUTTON", 23),
    ("TABLETPC", 86), ("XVIRTUALSCREEN", 76), ("YVIRTUALSCREEN", 77),
];

/// One extended event of `main.cpp:8-32`, with the number of arguments the
/// reference's message receiver passes when it calls the script member.
///
/// `deferred` marks the four names `checkExEvents` (`main.cpp:662-667`) caches
/// when `registerExEvent()` runs: the reference looks them up once, so adding
/// the method afterwards has no effect (`manual.tjs:153,160,176,271`).
struct WindowExEvent {
    name: &'static str,
    params: u8,
    deferred: bool,
}

const WINDOW_EX_EVENTS: &[WindowExEvent] = &[
    WindowExEvent { name: "onMinimize", params: 0, deferred: false },
    WindowExEvent { name: "onMaximize", params: 0, deferred: false },
    // `onMaximizeQuery` returns true to *refuse* the maximize (`main.cpp:513`).
    WindowExEvent { name: "onMaximizeQuery", params: 0, deferred: false },
    WindowExEvent { name: "onShow", params: 0, deferred: false },
    WindowExEvent { name: "onHide", params: 0, deferred: false },
    // The rect dictionary carries `type` for `onResizing`; a true return
    // writes the rectangle back (`main.cpp:537-538`, `:455-468`).
    WindowExEvent { name: "onResizing", params: 1, deferred: true },
    WindowExEvent { name: "onMoving", params: 1, deferred: true },
    WindowExEvent { name: "onMove", params: 2, deferred: true },
    WindowExEvent { name: "onMoveSizeBegin", params: 0, deferred: false },
    WindowExEvent { name: "onMoveSizeEnd", params: 0, deferred: false },
    WindowExEvent { name: "onDisplayChanged", params: 0, deferred: false },
    WindowExEvent { name: "onEnterMenuLoop", params: 0, deferred: false },
    WindowExEvent { name: "onExitMenuLoop", params: 0, deferred: false },
    WindowExEvent { name: "onActivateChanged", params: 2, deferred: false },
    WindowExEvent { name: "onScreenSave", params: 0, deferred: false },
    WindowExEvent { name: "onMonitorPower", params: 2, deferred: false },
    WindowExEvent { name: "onNcMouseMove", params: 4, deferred: true },
    WindowExEvent { name: "onNcMouseLeave", params: 0, deferred: false },
    WindowExEvent { name: "onNcMouseDown", params: 4, deferred: false },
    WindowExEvent { name: "onNcMouseUp", params: 4, deferred: false },
    WindowExEvent { name: "onExSystemMenuSelected", params: 1, deferred: false },
    WindowExEvent { name: "onStartKeyMenu", params: 2, deferred: false },
    WindowExEvent { name: "onAccelKeyMenu", params: 2, deferred: false },
    WindowExEvent { name: "onNonCapMouseEvent", params: 2, deferred: false },
    WindowExEvent { name: "onWindowsMessageHook", params: 4, deferred: false },
];

// ---------------------------------------------------------------------------
// Reference error texts (`main.cpp`, `TVPThrowExceptionMessage` call sites)
// ---------------------------------------------------------------------------

/// `main.cpp:698`, `:705`.
const CACHE_SETUP_FAILED: &str = "cache setup failed.";
/// `main.cpp:104`. (`:108` adds `icon not found.` for a file that exists but
/// carries no icon; without a platform icon loader this port cannot ask.)
const FILE_NOT_FOUND: &str = "file not found.";
/// `main.cpp:106`.
const CANNOT_GET_IN_ARCHIVE_ICON: &str = "cannot get in archive icon.";
/// `main.cpp:1155`.
const NO_LAYER_OBJECT: &str = "no layer object.";

/// `TJS_E_FAIL` (`tjsErrorDefs.h:39`, `-1`), which `TJSThrowFrom_tjs_error`
/// (`tjsError.cpp:265-271`) has no named case for: a script sees
/// `Unknown failure : FFFFFFFF` (`vc2012/string_table_en.rc:68`).
fn tjs_fail() -> TjsError {
    TjsError::runtime("Unknown failure : FFFFFFFF")
}

/// `!!value.AsInteger()` — the coercion the reference uses for its boolean
/// arguments (`main.cpp:200`, `:216`, `:235`, `:264`, `:328`, `:345`). It is
/// not TJS truthiness: a real `0.5` reads as false because `AsInteger`
/// truncates it, and an object raises the conversion error.
fn as_integer_flag(value: &Variant) -> Result<bool> {
    Ok(value.to_integer()? != 0)
}

/// `(tjs_int)value.AsInteger()` — TJS's value→number conversion, which for
/// strings is hex- and prefix-aware (`"0x10"` is 16, `"5abc"` is 5;
/// `runtime/value.rs` `string_to_integer`). Rust's `str::parse` is a different
/// conversion and must not stand in for it. A value that cannot convert at all
/// reads as 0.
fn as_integer_value(value: &Variant) -> i64 {
    value.to_integer().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Installation
// ---------------------------------------------------------------------------

/// Native method signature the plugin's handlers share.
type Native = fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, Vec<Variant>) -> Result<Variant>;

/// `Window` members the reference registers with `RawCallback`
/// (`main.cpp:1011-1033`) and `Method` (`:1035-1037`).
///
/// A `RawCallback` performs no argument-count check of its own (the handler
/// validates what it needs), which is why every entry but the two
/// notification lookups takes [`NativeArgCount::Any`]; `Method` rejects a call
/// with fewer arguments than the C++ signature declares
/// (`ncbind.hpp:1185`), so `getNotificationNum`/`getNotificationName` need one.
const WINDOW_METHODS: &[(&str, Native, NativeArgCount)] = &[
    ("minimize", window_minimize, NativeArgCount::Any),
    ("maximize", window_maximize, NativeArgCount::Any),
    ("showRestore", window_show_restore, NativeArgCount::Any),
    ("focusMenuByKey", window_focus_menu_by_key, NativeArgCount::Any),
    ("resetWindowIcon", window_reset_window_icon, NativeArgCount::Any),
    ("setWindowIcon", window_set_window_icon, NativeArgCount::Any),
    ("getWindowRect", window_get_window_rect, NativeArgCount::Any),
    ("getClientRect", window_get_client_rect, NativeArgCount::Any),
    ("getNormalRect", window_get_normal_rect, NativeArgCount::Any),
    ("setOverlayBitmap", window_set_overlay_bitmap, NativeArgCount::Any),
    ("ncHitTest", window_nc_hit_test, NativeArgCount::Any),
    ("setMessageHook", window_set_message_hook, NativeArgCount::Any),
    ("bringTo", window_bring_to, NativeArgCount::Any),
    ("sendToBack", window_send_to_back, NativeArgCount::Any),
    ("resetExSystemMenu", window_reset_ex_system_menu, NativeArgCount::Any),
    ("registerExEvent", window_register_ex_event, NativeArgCount::Any),
    ("getNotificationNum", window_get_notification_num, NativeArgCount::AtLeast(1)),
    ("getNotificationName", window_get_notification_name, NativeArgCount::AtLeast(1)),
];

/// `Window`'s read/write properties (`main.cpp:1013-1028`). The reference
/// backs them with the live Win32 window state; this port keeps the ones that
/// are plugin state and reports the ones that need a real window.
const WINDOW_PROPERTIES: &[&str] = &[
    "maximizeBox",
    "minimizeBox",
    "maximized",
    "minimized",
    "disableResize",
    "disableMove",
    "enableNCMouseEvent",
    "exSystemMenu",
];

/// `MenuItem`'s read/write properties (`main.cpp:1433-1436`).
const MENU_ITEM_PROPERTIES: &[&str] = &["rightJustify", "bmpItem", "bmpChecked", "bmpUnchecked"];

/// `System` members the reference attaches (`main.cpp:2088-2102`).
///
/// `clearGraphicCache` is the engine's own native method here
/// (`native/system.rs`), so the slot is normally taken and this entry only
/// covers a build without it. `setIconicPreview` is the one typed declaration
/// in the list (`static bool setIconicPreview(bool)`, `:2046`), so it rejects
/// a call without its flag.
const SYSTEM_METHODS: &[(&str, Native, NativeArgCount)] = &[
    ("getDisplayMonitors", system_get_display_monitors, NativeArgCount::Any),
    ("getMonitorInfo", system_get_monitor_info, NativeArgCount::Any),
    ("getCursorPos", system_get_cursor_pos, NativeArgCount::Any),
    ("setCursorPos", system_set_cursor_pos, NativeArgCount::Any),
    ("getSystemMetrics", system_get_system_metrics, NativeArgCount::Any),
    ("readEnvValue", system_read_env_value, NativeArgCount::Any),
    ("expandEnvString", system_expand_env_string, NativeArgCount::Any),
    ("setApplicationIcon", system_set_application_icon, NativeArgCount::Any),
    ("setIconicPreview", system_set_iconic_preview, NativeArgCount::AtLeast(1)),
    ("getDoubleClickTime", system_get_double_click_time, NativeArgCount::Any),
    ("breathe", system_breathe, NativeArgCount::Any),
    ("isBreathing", system_is_breathing, NativeArgCount::Any),
    ("clearGraphicCache", system_clear_graphic_cache, NativeArgCount::Any),
    ("getAboutString", system_get_about_string, NativeArgCount::Any),
    ("getCPUType", system_get_cpu_type, NativeArgCount::Any),
];

/// `Debug.console` members (`main.cpp:1620-1626`).
const CONSOLE_METHODS: &[(&str, Native, NativeArgCount)] = &[
    ("restoreMaximize", console_restore_maximize, NativeArgCount::Any),
    ("maximize", console_maximize, NativeArgCount::Any),
    ("getRect", console_get_rect, NativeArgCount::Any),
    ("setPos", console_set_pos, NativeArgCount::Any),
    ("getPlacement", console_get_placement, NativeArgCount::Any),
    ("setPlacement", console_set_placement, NativeArgCount::Any),
    ("bringAfter", console_bring_after, NativeArgCount::Any),
];

fn install_window_ex(runtime: &mut Runtime<KrkrHost>) {
    let Some(window) = global_object(runtime, "Window") else {
        runtime
            .host_mut()
            .log("windowEx.dll: this engine has no Window class; Window extensions not attached");
        return;
    };

    for (name, value) in WINDOW_HIT_TEST_CONSTANTS {
        set_constant_unless_present(runtime, window, name, Variant::Integer(*value));
    }
    for (name, function, arg_count) in WINDOW_METHODS {
        install_native_unless_present(runtime, window, name, *arg_count, *function);
    }
    for name in WINDOW_PROPERTIES {
        install_property_unless_present(
            runtime,
            window,
            name,
            window_property_get,
            window_property_set,
        );
    }
}

fn install_menu_item_ex(runtime: &mut Runtime<KrkrHost>) {
    let Some(menu_item) = global_object(runtime, "MenuItem") else {
        // `NCB_ATTACH_CLASS_WITH_HOOK` binds to the class that is already
        // registered (`ncbind.hpp:2097-2105` resolves it, and the reference has
        // no fallback for a missing one), so there is nothing to attach to.
        runtime.host_mut().log(
            "windowEx.dll: this engine has no MenuItem class; MenuItem extensions not attached",
        );
        return;
    };

    for (name, value) in MENU_ITEM_BITMAP_CONSTANTS {
        set_constant_unless_present(runtime, menu_item, name, Variant::Integer(*value));
    }
    install_native_unless_present(
        runtime,
        menu_item,
        "popupEx",
        NativeArgCount::Any,
        menu_item_popup_ex,
    );
    for name in MENU_ITEM_PROPERTIES {
        install_property_unless_present(
            runtime,
            menu_item,
            name,
            menu_item_property_get,
            menu_item_property_set,
        );
    }
}

fn install_pad_ex(runtime: &mut Runtime<KrkrHost>) {
    // `NCB_ATTACH_CLASS_WITH_HOOK(PadEx, Pad)` (`main.cpp:1749-1752`). The
    // engine registers no `Pad` class (krkr2's is `src/core/utils/PadIntf.cpp`),
    // so `registerExEvent` has nothing to attach to; the reference would look
    // the class up and use the NULL result.
    match global_object(runtime, "Pad") {
        Some(pad) => {
            install_native_unless_present(
                runtime,
                pad,
                "registerExEvent",
                NativeArgCount::Any,
                pad_register_ex_event,
            );
        }
        None => runtime.host_mut().log(
            "windowEx.dll: this engine has no Pad class; Pad.registerExEvent() not attached \
             (needs an engine-side Pad, krkr2 src/core/utils/PadIntf.cpp)",
        ),
    }
}

fn install_console_ex(runtime: &mut Runtime<KrkrHost>) {
    // `NCB_ATTACH_FUNCTION_WITHTAG(..., Debug.console, ...)`: the ncbind helper
    // resolves the target through `TVPExecuteExpression` and returns without
    // registering anything when it is not an object (`ncbind.hpp:2324-2345`),
    // which is what a build without console.dll does. This engine has no
    // `Debug.console` object, so the seven functions are not attached — the
    // reference is silent here, this port logs the reason once.
    let Some(debug) = global_object(runtime, "Debug") else {
        return;
    };
    let Some(console) = member_object(runtime, debug, "console") else {
        runtime.host_mut().log(
            "windowEx.dll: this engine has no Debug.console object; console extensions not \
             attached (needs a console window like console.dll)",
        );
        return;
    };
    for (name, function, arg_count) in CONSOLE_METHODS {
        install_native_unless_present(runtime, console, name, *arg_count, *function);
    }
}

fn install_system_ex(runtime: &mut Runtime<KrkrHost>) {
    let Some(system) = global_object(runtime, "System") else {
        runtime
            .host_mut()
            .log("windowEx.dll: this engine has no System object; System extensions not attached");
        return;
    };
    for (name, function, arg_count) in SYSTEM_METHODS {
        install_native_unless_present(runtime, system, name, *arg_count, *function);
    }
}

fn install_scripts_ex(runtime: &mut Runtime<KrkrHost>) {
    let Some(scripts) = global_object(runtime, "Scripts") else {
        runtime
            .host_mut()
            .log("windowEx.dll: this engine has no Scripts object; Scripts extensions not attached");
        return;
    };
    // The reference also replaces `Scripts.eval` with a wrapper that routes
    // through `TVPExecuteExpression` when `outputErrorLogOnEval` is false
    // (`main.cpp:2118-2129`). That override needs an engine-side expression
    // evaluator, so the engine's own `Scripts.eval` stays in place; the flag
    // this module keeps is therefore only observable through the setter's
    // return value, exactly like the reference's process-wide static before
    // any `Scripts.eval` call.
    runtime.host_mut().log(
        "windowEx.dll: Scripts.eval is not overridden (needs an engine expression evaluator); \
         Scripts.setEvalErrorLog only reports the previous flag value",
    );
    install_native_unless_present(
        runtime,
        scripts,
        "setEvalErrorLog",
        NativeArgCount::AtLeast(1),
        scripts_set_eval_error_log,
    );
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Capability names for the once-per-member gap log.
const CAP_WINDOW_OPS: &str = "desktop window operations (maximize/minimize/restore, z-order, icons)";
const CAP_ICONS: &str = "platform icon handling (an .ico loader plus WM_SETICON)";
const CAP_MONITORS: &str = "monitor enumeration";
const CAP_CURSOR: &str = "a host cursor position";
const CAP_METRICS: &str = "a host system-metrics source";
const CAP_MESSAGE_CHANNEL: &str = "a window message channel into TJS";
const CAP_MENUS: &str = "Win32 menu backing (Window.menu)";
const CAP_OVERLAY: &str = "an overlay child window";
const CAP_CONSOLE: &str = "a console window (console.dll's Debug.console)";

/// Members whose effect stops where engine support stops. The reference is
/// silent in the same situations (a call on an object without a window handle
/// is usually a no-op), so these logs are the only place the gap is visible;
/// each key fires once because a game can call these in a loop.
static GAP_LOG: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());

fn log_once(runtime: &mut Runtime<KrkrHost>, key: &'static str, message: impl FnOnce() -> String) {
    let first = match GAP_LOG.lock() {
        Ok(mut logged) => logged.insert(key),
        // A poisoned lock only costs the de-duplication.
        Err(_) => true,
    };
    if first {
        let message = message();
        runtime.host_mut().log(&message);
    }
}

fn log_gap_once(runtime: &mut Runtime<KrkrHost>, member: &'static str, capability: &str) {
    log_once(runtime, member, || {
        format!("windowEx.dll: {member} needs {capability}; validated and ignored")
    });
}

/// Resolves a global the way the reference's `GetGlobalObject` does
/// (`ncbind.hpp:2097-2105`), through the normal dispatch path so a lazily
/// published class resolves to its object on first touch.
fn global_object(runtime: &mut Runtime<KrkrHost>, name: &str) -> Option<ObjectHandle> {
    let global = runtime.global_handle();
    member_object(runtime, global, name)
}

fn member_object(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
) -> Option<ObjectHandle> {
    match runtime.resolve_object_member(object, name) {
        Ok(Variant::Object(handle)) => Some(handle),
        _ => None,
    }
}

/// Whether a member may still be registered: the reference writes its members
/// with `TJS_MEMBERENSURE`, i.e. it overwrites, but it loads before any script.
/// A module registered later in this engine can meet members the engine or a
/// script already published (`System.clearGraphicCache` is the engine's own),
/// so an occupied slot is left alone.
fn take_slot(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> bool {
    matches!(runtime.object_member(object, name), Variant::Void)
}

fn set_constant_unless_present(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
    value: Variant,
) {
    if take_slot(runtime, object, name) {
        runtime.set_object_member(object, name, value);
    }
}

fn install_native_unless_present(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &str,
    arg_count: NativeArgCount,
    function: Native,
) {
    if !take_slot(runtime, object, name) {
        return;
    }
    runtime.register_object_native_with_arg_count(object, name, arg_count, function);
}

fn install_property_unless_present(
    runtime: &mut Runtime<KrkrHost>,
    object: ObjectHandle,
    name: &'static str,
    get: fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, &'static str) -> Result<Variant>,
    set: fn(&mut Runtime<KrkrHost>, Option<ObjectHandle>, &'static str, Variant) -> Result<()>,
) {
    if !take_slot(runtime, object, name) {
        return;
    }
    runtime.register_object_native_property(
        object,
        name,
        move |runtime: &mut Runtime<KrkrHost>, this_obj: Option<ObjectHandle>| {
            get(runtime, this_obj, name)
        },
        move |runtime: &mut Runtime<KrkrHost>,
              this_obj: Option<ObjectHandle>,
              value: Variant| { set(runtime, this_obj, name, value) },
    );
}

/// The object a member call was made on. A self-bound closure carries its
/// receiver in the closure, which is what the engine's own property
/// accessors unwrap.
fn this_object(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>) -> Option<ObjectHandle> {
    this_obj.map(|handle| runtime.bound_this(handle).unwrap_or(handle))
}

/// `hasMember` (`main.cpp:411-414`): property lookup, class chain included.
fn member_in_class_chain(runtime: &Runtime<KrkrHost>, object: ObjectHandle, name: &str) -> bool {
    let mut current = Some(object);
    while let Some(handle) = current {
        if !matches!(runtime.object_member(handle, name), Variant::Void) {
            return true;
        }
        current = runtime.object_super_class(handle);
    }
    false
}

/// `IsInstanceOf(0, 0, 0, TJS_W(name), obj)` (`main.cpp:387`, `:908`): the
/// class name is looked up on the object and every class above it.
fn object_is_instance_of(
    runtime: &Runtime<KrkrHost>,
    object: ObjectHandle,
    class_name: &str,
) -> bool {
    let mut current = Some(object);
    while let Some(handle) = current {
        if runtime
            .object_class_infos(handle)
            .iter()
            .any(|info| info == class_name)
        {
            return true;
        }
        current = runtime.object_super_class(handle);
    }
    false
}

/// The receiver of a `Window` member, or `None` when there is no window to
/// work with — the class object itself and every non-`Window` receiver stand
/// in for the reference's "`GetHWND` found no handle" case
/// (`main.cpp:44-49`: the handle comes from the object's `HWND` property).
fn window_instance(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    let this = this_object(runtime, this_obj)?;
    if !object_is_instance_of(runtime, this, "Window") {
        return None;
    }
    if Some(this) == global_object(runtime, "Window") {
        return None;
    }
    Some(this)
}

/// `{x, y, w, h}` as `SetRect` builds it (`main.cpp:52-63`).
fn rect_object(runtime: &mut Runtime<KrkrHost>, x: i64, y: i64, w: i64, h: i64) -> ObjectHandle {
    let dict = runtime.alloc_dictionary_object();
    runtime.set_object_member(dict, "x", Variant::Integer(x));
    runtime.set_object_member(dict, "y", Variant::Integer(y));
    runtime.set_object_member(dict, "w", Variant::Integer(w));
    runtime.set_object_member(dict, "h", Variant::Integer(h));
    dict
}

/// Reads one of `Window`'s own numeric members (`left`, `top`, `width`,
/// `innerWidth`, …) through the dispatch path, so the engine's native
/// properties answer like they do for script.
fn window_number(runtime: &mut Runtime<KrkrHost>, window: ObjectHandle, name: &str) -> i64 {
    runtime
        .resolve_object_member(window, name)
        .ok()
        .and_then(|value| value.to_integer().ok())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Per-object state
// ---------------------------------------------------------------------------

/// The reference keeps its per-window state in the native `WindowEx` instance
/// (`main.cpp:807-818`) and its per-menu-item state in `MenuItemEx`
/// (`main.cpp:1230-1235`). A TJS object here has no native instance to hang
/// them on, so the state lives in one hidden member of the object it belongs
/// to; nothing else reads it, and a script that never touches these members
/// never creates it.
const WINDOW_EX_STATE: &str = "__windowExState";

fn state_object(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Option<ObjectHandle> {
    let this = this_object(runtime, this_obj)?;
    if let Variant::Object(state) = runtime.object_member(this, WINDOW_EX_STATE) {
        return Some(state);
    }
    let state = runtime.alloc_ordinary_object();
    runtime.set_object_member(this, WINDOW_EX_STATE, Variant::Object(state));
    Some(state)
}

fn state_value(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
) -> Variant {
    let Some(this) = this_object(runtime, this_obj) else {
        return Variant::Void;
    };
    let Variant::Object(state) = runtime.object_member(this, WINDOW_EX_STATE) else {
        return Variant::Void;
    };
    runtime.object_member(state, name)
}

fn set_state_value(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
    value: Variant,
) {
    if let Some(state) = state_object(runtime, this_obj) {
        runtime.set_object_member(state, name, value);
    }
}

fn state_flag(runtime: &Runtime<KrkrHost>, this_obj: Option<ObjectHandle>, name: &str) -> bool {
    state_value(runtime, this_obj, name).is_truthy()
}

/// `DWORD bitHooks[0x0400 / 32]` (`main.cpp:818`): one bit per message number,
/// stored as a 32-element integer array.
const MESSAGE_HOOK_WORDS: usize = 0x0400 / 32;

fn message_hook_words(
    runtime: &Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
) -> Vec<i64> {
    let mut words = match state_value(runtime, this_obj, "messageHooks") {
        Variant::Object(handle) => runtime
            .array_elements(handle)
            .map(|elements| {
                elements
                    .iter()
                    .map(|value| value.to_integer().unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    words.resize(MESSAGE_HOOK_WORDS, 0);
    words
}

fn store_message_hook_words(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    words: Vec<i64>,
) -> bool {
    let enabled = words.iter().any(|word| *word != 0);
    let array = runtime.alloc_array_object(
        words
            .into_iter()
            .map(Variant::Integer)
            .collect::<Vec<_>>(),
    );
    set_state_value(runtime, this_obj, "messageHooks", Variant::Object(array));
    enabled
}

/// `WindowEx::setMessageHookOnel` (`main.cpp:675-688`).
fn set_message_hook_one(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    on: bool,
    number: i64,
) -> bool {
    let index = (number / 32) as usize;
    let bit = 1i64 << (number % 32);
    let mut words = message_hook_words(runtime, this_obj);
    words[index] = if on {
        words[index] | bit
    } else {
        words[index] & !bit
    };
    store_message_hook_words(runtime, this_obj, words)
}

/// `WindowEx::setMessageHookAll` (`main.cpp:689-693`): every bit on or off,
/// and the returned flag is the requested state.
fn set_message_hook_all(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    on: bool,
) -> bool {
    let word = if on { -1 } else { 0 };
    store_message_hook_words(
        runtime,
        this_obj,
        vec![word; MESSAGE_HOOK_WORDS],
    );
    on
}

// ---------------------------------------------------------------------------
// Window._Notifications and the two notification lookups
// ---------------------------------------------------------------------------

const NOTIFICATIONS_MEMBER: &str = "_Notifications";

/// `WindowEx::_getNotificationVariant` (`main.cpp:726-805`): the table lives on
/// the global `Window` and is built once; a script that replaced
/// `Window._Notifications` owns the lookup from then on. A table that cannot be
/// produced raises the reference's `cache setup failed.` (`:698`, `:705`).
fn notification_table(runtime: &mut Runtime<KrkrHost>) -> Result<Variant> {
    let Some(window) = global_object(runtime, "Window") else {
        return Err(TjsError::runtime(CACHE_SETUP_FAILED));
    };
    if runtime.has_object_member(window, NOTIFICATIONS_MEMBER) {
        return Ok(runtime.object_member(window, NOTIFICATIONS_MEMBER));
    }
    let dict = runtime.alloc_dictionary_object();
    for (name, value) in WINDOW_NOTIFICATIONS {
        runtime.set_object_member(dict, *name, Variant::Integer(*value));
        runtime.set_object_member(
            dict,
            value.to_string(),
            Variant::String((*name).to_string()),
        );
    }
    runtime.set_object_member(window, NOTIFICATIONS_MEMBER, Variant::Object(dict));
    Ok(Variant::Object(dict))
}

/// `ncbPropAccessor::getIntValue(key, -1)` (`main.cpp:695-701`).
fn notification_number(runtime: &mut Runtime<KrkrHost>, name: &str) -> Result<i64> {
    let table = notification_table(runtime)?;
    let Some(table) = table.object_handle() else {
        return Ok(-1);
    };
    match runtime.resolve_object_member(table, name) {
        Ok(Variant::Void) | Err(_) => Ok(-1),
        Ok(Variant::Integer(value)) => Ok(value),
        // `ncbPropAccessor::getIntValue` converts through `(tjs_int)`, i.e.
        // TJS's own string→number conversion — `"0x10"` is 16 and `"5abc"` is 5
        // (`runtime/value.rs` `string_to_integer`), never Rust's `str::parse`.
        Ok(value) => Ok(as_integer_value(&value)),
    }
}

/// `ncbPropAccessor::getStrValue(number)`. The reverse entries are stored under
/// the decimal member name, which is also how this engine's VM resolves a
/// numeric index (`vm/dispatch.rs:278`), so `Window._Notifications[5]` and
/// `getNotificationName(5)` see the same member.
fn notification_name(runtime: &mut Runtime<KrkrHost>, number: i64) -> Result<String> {
    let table = notification_table(runtime)?;
    let Some(table) = table.object_handle() else {
        return Ok(String::new());
    };
    match runtime.resolve_object_member(table, &number.to_string()) {
        Ok(Variant::Void) | Err(_) => Ok(String::new()),
        Ok(Variant::String(value)) => Ok(value),
        Ok(value) => Ok(value.to_tjs_string().unwrap_or_default()),
    }
}

fn window_get_notification_num(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let key = args
        .first()
        .map(Variant::to_tjs_string)
        .transpose()?
        .unwrap_or_default();
    Ok(Variant::Integer(notification_number(runtime, &key)?))
}

fn window_get_notification_name(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let number = args
        .first()
        .map(Variant::to_integer)
        .transpose()?
        .unwrap_or(0);
    Ok(Variant::String(notification_name(runtime, number)?))
}

/// `WindowEx::checkExEvents` (`main.cpp:662-667`): the four deferred event
/// names are looked up once, when the script asks for the extended events.
/// Nothing delivers messages here yet, so a handler that *is* present is
/// reported once together with the argument shape the dispatcher must use.
fn window_register_ex_event(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    if let Some(this) = this_object(runtime, this_obj) {
        for event in WINDOW_EX_EVENTS.iter().filter(|event| event.deferred) {
            let present = member_in_class_chain(runtime, this, event.name);
            set_state_flag(runtime, Some(this), event.name, present);
            if present {
                log_once(runtime, event.name, || {
                    format!(
                        "windowEx.dll: {} handler present ({} argument(s)); no window message \
                         channel delivers it yet",
                        event.name, event.params
                    )
                });
            }
        }
    }
    Ok(Variant::Void)
}

fn set_state_flag(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &str,
    value: bool,
) {
    set_state_value(runtime, this_obj, name, Variant::Integer(i64::from(value)));
}

// ---------------------------------------------------------------------------
// Window members
// ---------------------------------------------------------------------------

/// `postSysCommand` (`main.cpp:76-79`) posts an `SC_*` command to the window's
/// handle. With no handle the post has no receiver — the reference's own
/// outcome for a `Window` object without an `HWND`.
fn window_post_sys_command(runtime: &mut Runtime<KrkrHost>, member: &'static str) -> Variant {
    log_gap_once(runtime, member, CAP_WINDOW_OPS);
    Variant::Void
}

fn window_minimize(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `SC_MINIMIZE` (`main.cpp:85`).
    Ok(window_post_sys_command(runtime, "Window.minimize"))
}

fn window_maximize(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `SC_MAXIMIZE` (`main.cpp:86`).
    Ok(window_post_sys_command(runtime, "Window.maximize"))
}

fn window_show_restore(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `SC_RESTORE` (`main.cpp:87`).
    Ok(window_post_sys_command(runtime, "Window.showRestore"))
}

fn window_focus_menu_by_key(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `SC_KEYMENU` with the key as the message parameter (`main.cpp:88`). The
    // reference dereferences `param[0]` even when no argument was passed, so
    // an argument-less call is as undefined there as it is harmless here.
    Ok(window_post_sys_command(runtime, "Window.focusMenuByKey"))
}

fn window_reset_window_icon(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `_resetWindowIcon` (`main.cpp:124-131`) posts `WM_SETICON`; it does
    // nothing when there is no handle.
    log_gap_once(runtime, "Window.resetWindowIcon", CAP_ICONS);
    Ok(Variant::Void)
}

fn window_set_window_icon(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `setWindowIcon(file, withapp)` (`main.cpp:96-123`). The storage lookup
    // happens before the window is touched and is reproduced here: an empty
    // string is the documented "reset" form and skips it entirely.
    if let Some(Variant::String(file)) = args.first() {
        if !file.is_empty() {
            resolve_icon_storage(runtime, file)?;
        }
    }
    // `p[1]->operator bool()` — truthiness, not `AsInteger` (`main.cpp:98`).
    let _with_app = args.get(1).map(Variant::is_truthy).unwrap_or(false);
    log_gap_once(runtime, "Window.setWindowIcon", CAP_ICONS);
    Ok(Variant::Void)
}

/// `_loadExternalIcon` (`main.cpp:100-111`, the krkr2 line): a name that does
/// not resolve at all is `file not found.`, and a name that resolves but has no
/// *locally accessible* form is `cannot get in archive icon.` — the icon is
/// extracted from a file on disk, so an XP3 member or a `media://` or
/// memory-backed resource cannot yield one. `KrkrHost::placed_path`
/// (`host.rs:1009-1015`) is exactly that test: it answers `None` for everything
/// that is not a local file.
fn resolve_icon_storage(runtime: &mut Runtime<KrkrHost>, file: &str) -> Result<()> {
    match runtime.host().placed_storage_name(file) {
        None => Err(TjsError::runtime(FILE_NOT_FOUND)),
        Some(_) if runtime.host().placed_path(file).is_none() => {
            Err(TjsError::runtime(CANNOT_GET_IN_ARCHIVE_ICON))
        }
        Some(_) => Ok(()),
    }
}

fn window_get_window_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = window_instance(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    // `GetWindowRect` (`main.cpp:138-147`). `Window.left/top/width/height` are
    // the same outer rectangle: the official getters read `GetWindowRect`
    // (krkrz `src/core/environ/win32/TVPWindow.cpp:636-725`), and this engine's
    // window model stores them.
    let x = window_number(runtime, this, "left");
    let y = window_number(runtime, this, "top");
    let w = window_number(runtime, this, "width");
    let h = window_number(runtime, this, "height");
    Ok(Variant::Object(rect_object(runtime, x, y, w, h)))
}

fn window_get_client_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = window_instance(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    // `GetClientRect` + `ClientToScreen` (`main.cpp:150-163`).
    // `Window.innerWidth/innerHeight` is the client size in the official
    // interface (krkrz `src/core/visual/WindowIntf.cpp:1559` reads
    // `GetInnerWidth`, which is `GetClientRect().width`); the engine's window
    // has no non-client border, so the client origin is the window origin.
    let x = window_number(runtime, this, "left");
    let y = window_number(runtime, this, "top");
    let w = window_number(runtime, this, "innerWidth");
    let h = window_number(runtime, this, "innerHeight");
    Ok(Variant::Object(rect_object(runtime, x, y, w, h)))
}

fn window_get_normal_rect(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let Some(this) = window_instance(runtime, this_obj) else {
        return Ok(Variant::Void);
    };
    // `nofix` (`main.cpp:181`) drops the work-area correction the reference
    // applies to `WINDOWPLACEMENT.rcNormalPosition`; the engine has no monitor
    // work area, so both forms report the same rectangle — and with no
    // maximize state the restored rectangle is the current one.
    let _nofix = args.first().map(as_integer_flag).transpose()?.unwrap_or(false);
    let x = window_number(runtime, this, "left");
    let y = window_number(runtime, this, "top");
    let w = window_number(runtime, this, "width");
    let h = window_number(runtime, this, "height");
    Ok(Variant::Object(rect_object(runtime, x, y, w, h)))
}

fn window_nc_hit_test(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `nonClientHitTest` (`main.cpp:332-339`): fewer than two parameters is a
    // count error, and both are converted before the message is sent.
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    let _x = args[0].to_integer()? & 0xFFFF;
    let _y = args[1].to_integer()? & 0xFFFF;
    // `*r = (tjs_int)::SendMessage(GetHWND(obj), WM_NCHITTEST, 0, …)`
    // (`main.cpp:337`) reads the handle off the object's `HWND` property
    // (`:47-51`). Nothing in this engine ever writes `Window.HWND`, so the
    // reference's own null-handle path is the one that applies: the message
    // goes nowhere and `SendMessage` answers 0 — never an `HT*` code, which a
    // window-less object cannot produce.
    Ok(Variant::Integer(0))
}

fn window_set_message_hook(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `setMessageHook(on, ev)` (`main.cpp:341-362`). The flag is
    // `!!p[0]->AsInteger()`; `ev` is a notification *name* (without the `WM_`
    // prefix, e.g. `"SIZE"`) or a message number, and anything outside
    // `0..0x3FF` is `TJS_E_FAIL` (`:355`).
    let on = args.first().map(as_integer_flag).transpose()?.unwrap_or(false);
    let enabled = match args.get(1) {
        None => set_message_hook_all(runtime, this_obj, on),
        Some(event) => {
            let number = match event {
                Variant::String(name) => notification_number(runtime, name)?,
                other => other.to_integer()?,
            };
            if !(0..0x400).contains(&number) {
                return Err(tjs_fail());
            }
            set_message_hook_one(runtime, this_obj, on, number)
        }
    };
    Ok(Variant::Integer(i64::from(enabled)))
}

fn window_bring_to(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `SetWindowPos` z-ordering (`main.cpp:364-397`). The argument is only
    // parsed inside the `hwnd != NULL` branch, so there is nothing to validate
    // for a window without a handle.
    log_gap_once(runtime, "Window.bringTo", CAP_WINDOW_OPS);
    Ok(Variant::Void)
}

fn window_send_to_back(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `HWND_BOTTOM` (`main.cpp:398-405`).
    log_gap_once(runtime, "Window.sendToBack", CAP_WINDOW_OPS);
    Ok(Variant::Void)
}

fn window_reset_ex_system_menu(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `_resetExSystemMenu` (`main.cpp:310-317`) returns early unless a
    // modified system menu exists; rebuilding one needs the Win32 menu.
    log_gap_once(runtime, "Window.resetExSystemMenu", CAP_MENUS);
    Ok(Variant::Void)
}

fn window_set_overlay_bitmap(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `setOverlayBitmap(layer)` (`main.cpp:282-286`, `:711-724`): a non-object
    // (void included) hides an existing overlay and succeeds, an object that is
    // not a `Layer` fails with `TJS_E_FAIL` (`:718-721` + `:907-908`).
    let Some(layer) = args.first().and_then(Variant::object_handle) else {
        return Ok(Variant::Void);
    };
    if !object_is_instance_of(runtime, layer, "Layer") {
        return Err(tjs_fail());
    }
    log_gap_once(runtime, "Window.setOverlayBitmap", CAP_OVERLAY);
    Ok(Variant::Void)
}

/// `Window`'s property getters (`main.cpp:193-330`).
fn window_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &'static str,
) -> Result<Variant> {
    match name {
        // `GetWindowLong` on a null handle is 0, and `IsZoomed`/`IsIconic`
        // answer false for one (`main.cpp:194-196`, `:210-212`, `:226-229`,
        // `:241-244`). A window that can never be maximized reads the same.
        "maximizeBox" | "minimizeBox" | "maximized" | "minimized" => Ok(Variant::Integer(0)),
        // The three flags the reference stores in its native instance
        // (`main.cpp:258`, `:271`, `:322`, `:1243-1245`); they start false.
        "disableResize" | "disableMove" | "enableNCMouseEvent" => {
            Ok(Variant::Integer(i64::from(state_flag(runtime, this_obj, name))))
        }
        // `getExSystemMenu` hands back the stored object; a write of a
        // non-object leaves the reference's member NULL, which reads as void
        // (`main.cpp:289-293`, `:301`).
        "exSystemMenu" => Ok(match state_value(runtime, this_obj, name) {
            value @ (Variant::Object(_) | Variant::Closure(_)) => value,
            _ => Variant::Void,
        }),
        _ => Ok(Variant::Void),
    }
}

/// `Window`'s property setters (`main.cpp:193-330`).
fn window_property_set(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &'static str,
    value: Variant,
) -> Result<()> {
    match name {
        // `setDisableResize`/`setDisableMove`/`setEnNCMEvent` store
        // `!!p[0]->AsInteger()` (`main.cpp:261-266`, `:274-280`, `:325-330`);
        // the value only influences message handling.
        "disableResize" | "disableMove" | "enableNCMouseEvent" => {
            set_state_flag(runtime, this_obj, name, as_integer_flag(&value)?);
        }
        // The window-style writes and the `SC_MAXIMIZE`/`SC_RESTORE` commands
        // (`main.cpp:199-253`) need the live window. The argument is converted
        // the reference's way first, so an object argument still fails with the
        // same conversion error.
        "maximizeBox" | "minimizeBox" | "maximized" | "minimized" => {
            let _requested = as_integer_flag(&value)?;
            let member = match name {
                "maximizeBox" => "Window.maximizeBox",
                "minimizeBox" => "Window.minimizeBox",
                "maximized" => "Window.maximized",
                _ => "Window.minimized",
            };
            log_gap_once(runtime, member, CAP_WINDOW_OPS);
        }
        // `setExSystemMenu` (`main.cpp:294-304`) keeps the script object and
        // rebuilds the menu from it; the object is kept here so the getter
        // matches, and the rebuild reports the gap.
        "exSystemMenu" => {
            let stored = match value {
                value @ (Variant::Object(_) | Variant::Closure(_)) => value,
                _ => Variant::Void,
            };
            set_state_value(runtime, this_obj, "exSystemMenu", stored);
            log_gap_once(runtime, "Window.exSystemMenu", CAP_MENUS);
        }
        _ => {}
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// MenuItem members
// ---------------------------------------------------------------------------

/// `MenuItem`'s property getters (`main.cpp:1116-1142`).
fn menu_item_property_get(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &'static str,
) -> Result<Variant> {
    match name {
        // `getRightJustify` answers `rj > 0`, and the reference's instance
        // initializes `rj` to -1, so an untouched item reads false
        // (`main.cpp:1117`, `:1222`).
        "rightJustify" => Ok(Variant::Integer(i64::from(state_flag(runtime, this_obj, name)))),
        // `getBmpSelect` (`main.cpp:1136-1142`): an empty slot is 0, a Win32
        // handle value reads back as itself, and a Layer-derived bitmap reads
        // -1.
        _ => Ok(Variant::Integer(menu_item_bitmap_value(&state_value(
            runtime, this_obj, name,
        )))),
    }
}

fn menu_item_bitmap_value(state: &Variant) -> i64 {
    match state {
        Variant::Integer(value) => *value,
        Variant::Object(_) | Variant::Closure(_) => -1,
        _ => 0,
    }
}

/// `MenuItem`'s property setters (`main.cpp:1118-1165`).
fn menu_item_property_set(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    name: &'static str,
    value: Variant,
) -> Result<()> {
    match name {
        "rightJustify" => {
            // `setRightJustify` (`main.cpp:1118-1122`) coerces with
            // `AsInteger`, stores the flag and then updates the Win32 menu
            // item and the menu bar — `updateMenuItemInfo` throws
            // `Cannot get parent menu.` when the item has no parent `HMENU`
            // (`:1188-1197`), which no engine object has, so the flag is kept
            // and the update is reported as a gap.
            set_state_flag(runtime, this_obj, name, as_integer_flag(&value)?);
        }
        _ => {
            let stored = match &value {
                // `setBmpSelect` (`main.cpp:1143-1165`) stores
                // `(HBITMAP)v.AsInteger()` for void, integer and string
                // arguments — TJS's conversion, so the string `"0x10"` stores
                // 16…
                Variant::Void => Variant::Integer(0),
                Variant::Integer(number) => Variant::Integer(*number),
                Variant::String(_) => Variant::Integer(as_integer_value(&value)),
                Variant::Object(_) | Variant::Closure(_) => {
                    let Some(layer) = value.object_handle() else {
                        return Ok(());
                    };
                    // …throws for an object that is not a `Layer`…
                    if !object_is_instance_of(runtime, layer, "Layer") {
                        return Err(TjsError::runtime(NO_LAYER_OBJECT));
                    }
                    // …and converts a `Layer` to a bitmap handle, whose slot
                    // reads back as -1 (`main.cpp:1139`, `:1152-1161`).
                    Variant::Object(layer)
                }
                // A real (or any other type) matches no case, so the slot ends
                // up empty and reads 0.
                _ => Variant::Void,
            };
            set_state_value(runtime, this_obj, name, stored);
        }
    }
    let member = match name {
        "rightJustify" => "MenuItem.rightJustify",
        "bmpChecked" => "MenuItem.bmpChecked",
        "bmpUnchecked" => "MenuItem.bmpUnchecked",
        _ => "MenuItem.bmpItem",
    };
    log_gap_once(runtime, member, CAP_MENUS);
    Ok(())
}

fn menu_item_popup_ex(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `popupEx(flags, x, y, window, rect, menulist)` (`main.cpp:1369-1408`)
    // builds a Win32 menu from the list and tracks it; the selection comes back
    // as the MenuItem object. Building the menu is what needs the engine's menu
    // backing, and with no menu to show the reference's own outcome is the
    // cleared result — void.
    log_gap_once(runtime, "MenuItem.popupEx", CAP_MENUS);
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// Pad / Console
// ---------------------------------------------------------------------------

fn pad_register_ex_event(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `PadEx::registerExEvents` → `regist(true)` (`main.cpp:1688-1718`) looks
    // up the `TTVPPadForm` window and throws when it is not there. This engine
    // has no Pad window, so the reference's error is the faithful answer.
    Err(TjsError::runtime("Cannot get Pad window handle."))
}

fn console_restore_maximize(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `ConsoleEx::restoreMaximize` (`main.cpp:1511-1517`) reports whether a
    // console window exists; without one it answers false.
    log_gap_once(runtime, "Debug.console.restoreMaximize", CAP_CONSOLE);
    Ok(Variant::Integer(0))
}

fn console_maximize(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `ConsoleEx::maximize` (`main.cpp:1518-1524`).
    log_gap_once(runtime, "Debug.console.maximize", CAP_CONSOLE);
    Ok(Variant::Integer(0))
}

fn console_get_rect(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `ConsoleEx::getRect` (`main.cpp:1574-1583`): a failed `GetWindowRect`
    // leaves the result cleared.
    log_gap_once(runtime, "Debug.console.getRect", CAP_CONSOLE);
    Ok(Variant::Void)
}

fn console_set_pos(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `ConsoleEx::setPos` (`main.cpp:1585-1604`).
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    let _x = args[0].to_integer()?;
    let _y = args[1].to_integer()?;
    log_gap_once(runtime, "Debug.console.setPos", CAP_CONSOLE);
    Ok(Variant::Void)
}

fn console_get_placement(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `ConsoleEx::getPlacement` (`main.cpp:1526-1547`).
    log_gap_once(runtime, "Debug.console.getPlacement", CAP_CONSOLE);
    Ok(Variant::Void)
}

fn console_set_placement(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `ConsoleEx::setPlacement` (`main.cpp:1549-1572`): the count and the
    // dictionary type are checked before the window is touched.
    let Some(dict) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    if dict.object_handle().is_none() {
        return Err(TjsError::invalid_param());
    }
    log_gap_once(runtime, "Debug.console.setPlacement", CAP_CONSOLE);
    Ok(Variant::Integer(0))
}

fn console_bring_after(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `ConsoleEx::bringAfter` (`main.cpp:1605-1618`).
    log_gap_once(runtime, "Debug.console.bringAfter", CAP_CONSOLE);
    Ok(Variant::Void)
}

// ---------------------------------------------------------------------------
// System members
// ---------------------------------------------------------------------------

fn system_get_display_monitors(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `getDisplayMonitors(x, y, w, h)` (`main.cpp:1826-1849`): zero or four
    // parameters, anything else is a count error, and the four are converted
    // before the enumeration.
    if !matches!(args.len(), 0 | 4) {
        return Err(TjsError::bad_param_count());
    }
    for value in &args {
        let _ = value.to_integer()?;
    }
    // `EnumDisplayMonitors` with no host monitor backend enumerates nothing;
    // the reference still hands back the array it filled.
    log_gap_once(runtime, "System.getDisplayMonitors", CAP_MONITORS);
    Ok(Variant::Object(runtime.alloc_array_object(Vec::new())))
}

fn system_get_monitor_info(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `getMonitorInfo(near, …)` (`main.cpp:1852-1893`). `near` is read before
    // the parameter count is checked; the two-parameter form needs a `Window`
    // object, which is the one type check in the member.
    let _near = if args.is_empty() {
        false
    } else {
        args[0].to_integer()? != 0
    };
    match args.len() {
        0 => {}
        2 => {
            let window = args[1]
                .object_handle()
                .filter(|handle| object_is_instance_of(runtime, *handle, "Window"));
            if window.is_none() {
                return Err(TjsError::invalid_param());
            }
        }
        3 => {
            let _x = args[1].to_integer()?;
            let _y = args[2].to_integer()?;
        }
        5 => {
            for value in &args[1..5] {
                let _ = value.to_integer()?;
            }
        }
        _ => return Err(TjsError::bad_param_count()),
    }
    // No monitor backend: `MonitorFromPoint`-style lookups find nothing and
    // the result stays cleared.
    log_gap_once(runtime, "System.getMonitorInfo", CAP_MONITORS);
    Ok(Variant::Void)
}

fn system_get_cursor_pos(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `getCursorPos` (`main.cpp:1896-1908`): a failed `GetCursorPos` clears the
    // result. The engine's cursor position is host-internal
    // (`KrkrHost::cursor_position`) and not reachable from a plugin.
    log_gap_once(runtime, "System.getCursorPos", CAP_CURSOR);
    Ok(Variant::Void)
}

fn system_set_cursor_pos(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `setCursorPos(x, y)` (`main.cpp:1909-1918`): two parameters required, and
    // the answer is whether the platform call succeeded.
    if args.len() < 2 {
        return Err(TjsError::bad_param_count());
    }
    let _x = args[0].to_integer()?;
    let _y = args[1].to_integer()?;
    log_gap_once(runtime, "System.setCursorPos", CAP_CURSOR);
    Ok(Variant::Integer(0))
}

fn system_get_system_metrics(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `getSystemMetrics(index)` (`main.cpp:1919-1974`): the index is the `SM_*`
    // name without its prefix, case-insensitive; anything unknown, empty or
    // non-string is `TJS_E_INVALIDPARAM`, and the table is built on first use
    // as `System.metrics`.
    let Some(key) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let Variant::String(key) = key else {
        return Err(TjsError::invalid_param());
    };
    if key.is_empty() {
        return Err(TjsError::invalid_param());
    }
    let key = key.to_ascii_uppercase();
    let metrics = system_metrics_table(runtime)?;
    // `ncbPropAccessor::getIntValue(key, -1)` (`main.cpp:1970`): a name that is
    // not in the table reads back as -1 (this engine's Dictionary miss reads as
    // void) and is rejected right after.
    let index = match runtime.resolve_object_member(metrics, &key) {
        Ok(Variant::Integer(value)) => value,
        Ok(Variant::Void) | Err(_) => -1,
        Ok(value) => value.to_integer().unwrap_or(0),
    };
    if index < 0 {
        return Err(TjsError::invalid_param());
    }
    match host_metric_value(runtime, this_obj, index) {
        Some(value) => Ok(Variant::Integer(value)),
        None => {
            log_gap_once(runtime, "System.getSystemMetrics", CAP_METRICS);
            Ok(Variant::Integer(0))
        }
    }
}

/// `System.metrics` (`main.cpp:1936-1968`), cached on the `System` object the
/// way the reference caches it.
fn system_metrics_table(runtime: &mut Runtime<KrkrHost>) -> Result<ObjectHandle> {
    let Some(system) = global_object(runtime, "System") else {
        return Err(tjs_fail());
    };
    if runtime.has_object_member(system, "metrics") {
        return runtime
            .object_member(system, "metrics")
            .object_handle()
            .ok_or_else(TjsError::invalid_param);
    }
    let dict = runtime.alloc_dictionary_object();
    for (name, value) in SYSTEM_METRICS {
        runtime.set_object_member(dict, *name, Variant::Integer(*value));
    }
    runtime.set_object_member(system, "metrics", Variant::Object(dict));
    Ok(dict)
}

/// The metrics this engine can answer. It publishes the screen and
/// virtual-desktop geometry on `System` (`engine.rs:4395-4413`), which are
/// exactly `SM_CXSCREEN`/`SM_CYSCREEN` and the four virtual-screen metrics;
/// every other index has no host source yet.
fn host_metric_value(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    index: i64,
) -> Option<i64> {
    let member = match index {
        0 => "screenWidth",
        1 => "screenHeight",
        76 => "desktopLeft",
        77 => "desktopTop",
        78 => "desktopWidth",
        79 => "desktopHeight",
        _ => return None,
    };
    let system = this_object(runtime, this_obj).or_else(|| global_object(runtime, "System"))?;
    runtime
        .object_member(system, member)
        .to_integer()
        .ok()
}

fn system_read_env_value(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `readEnvValue(name)` (`main.cpp:1977-1996`): a missing, non-string or
    // empty name is rejected, an unset variable reads as void.
    let Some(name) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let Variant::String(name) = name else {
        return Err(TjsError::invalid_param());
    };
    if name.is_empty() {
        return Err(TjsError::invalid_param());
    }
    Ok(match std::env::var_os(name) {
        Some(value) => Variant::String(value.to_string_lossy().into_owned()),
        None => Variant::Void,
    })
}

fn system_expand_env_string(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `expandEnvString(text)` (`main.cpp:1998-2015`) runs
    // `ExpandEnvironmentStrings`: `%name%` is replaced when the variable
    // exists and left as written when it does not.
    let Some(text) = args.first() else {
        return Err(TjsError::bad_param_count());
    };
    let text = match text {
        Variant::String(value) => value.clone(),
        value => value.to_tjs_string().unwrap_or_default(),
    };
    Ok(Variant::String(expand_environment_variables(&text)))
}

fn expand_environment_variables(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        result.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            // A variable name: `%name%`.
            Some(end) if end > 0 => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) => result.push_str(&value),
                    // An undefined name keeps its `%…%` spelling.
                    Err(_) => result.push_str(&rest[start..start + end + 2]),
                }
                rest = &after[end + 1..];
            }
            // A lone `%` (or `%%`) is not a variable reference.
            _ => {
                result.push('%');
                rest = after;
            }
        }
    }
    result.push_str(rest);
    result
}

fn system_set_application_icon(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `setApplicationIcon(file)` (`main.cpp:2018-2021`): a string argument goes
    // through `_loadExternalIcon` and its storage errors; anything else resets
    // the icon to the executable's.
    if let Some(Variant::String(file)) = args.first() {
        if !file.is_empty() {
            resolve_icon_storage(runtime, file)?;
        }
    }
    log_gap_once(runtime, "System.setApplicationIcon", CAP_ICONS);
    Ok(Variant::Void)
}

fn system_set_iconic_preview(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    // `setIconicPreview(iconic)` (`main.cpp:2046-2073`) answers whether
    // `DwmSetWindowAttribute` succeeded; without the DWM API that is false,
    // which is also what it returns on Windows XP.
    let _iconic = args[0].is_truthy();
    log_gap_once(runtime, "System.setIconicPreview", CAP_WINDOW_OPS);
    Ok(Variant::Integer(0))
}

fn system_get_double_click_time(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `GetDoubleClickTime()` (`main.cpp:2076`). The engine has no host setting
    // for it, so the documented default answers (`manual.tjs:519-525`).
    log_gap_once(runtime, "System.getDoubleClickTime", CAP_METRICS);
    Ok(Variant::Integer(500))
}

fn system_breathe(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `TVPBreathe()` pumps the platform message queue with events disabled
    // (`EventImpl.cpp:76-94`); the host owns message pumping here.
    log_gap_once(runtime, "System.breathe", CAP_MESSAGE_CHANNEL);
    Ok(Variant::Void)
}

fn system_is_breathing(
    _runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `TVPGetBreathing()` is only true while a `breathe()` call is on the
    // stack, which a script calling it directly can never observe.
    Ok(Variant::Integer(0))
}

fn system_clear_graphic_cache(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // The engine registers its own `System.clearGraphicCache`
    // (`native/system.rs`), so this handler is only reached if that ever goes
    // away.
    log_gap_once(runtime, "System.clearGraphicCache", CAP_WINDOW_OPS);
    Ok(Variant::Void)
}

fn system_get_about_string(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `TVPGetAboutString()` (`MsgIntf.cpp:163-177`) renders the engine's own
    // version resource plus the important log; this engine has no such
    // resource, so it answers its own identity.
    log_gap_once(runtime, "System.getAboutString", CAP_METRICS);
    Ok(Variant::String(
        "Kirakira (Kirikiri-compatible emulator)".to_string(),
    ))
}

fn system_get_cpu_type(
    runtime: &mut Runtime<KrkrHost>,
    _this_obj: Option<ObjectHandle>,
    _args: Vec<Variant>,
) -> Result<Variant> {
    // `TVPGetCPUType()` returns the `TVP_CPU_HAS_*` feature bits of the host
    // CPU (`DetectCPU.cpp:372-376`); no such detection exists engine-side yet.
    log_gap_once(runtime, "System.getCPUType", CAP_METRICS);
    Ok(Variant::Integer(0))
}

// ---------------------------------------------------------------------------
// Scripts members
// ---------------------------------------------------------------------------

/// `Scripts.outputErrorLogOnEval` (`main.cpp:2108-2115`): the reference keeps a
/// process-wide static, initialized to true, and `setEvalErrorLog` answers the
/// value it replaced. The flag is stored on the `Scripts` object here, which is
/// the per-runtime equivalent of that static.
const EVAL_ERROR_LOG_STATE: &str = "evalErrorLog";

fn scripts_set_eval_error_log(
    runtime: &mut Runtime<KrkrHost>,
    this_obj: Option<ObjectHandle>,
    args: Vec<Variant>,
) -> Result<Variant> {
    let previous = match state_value(runtime, this_obj, EVAL_ERROR_LOG_STATE) {
        Variant::Void => Variant::Integer(1),
        value => value,
    };
    let enabled = args[0].is_truthy();
    set_state_value(
        runtime,
        this_obj,
        EVAL_ERROR_LOG_STATE,
        Variant::Integer(i64::from(enabled)),
    );
    Ok(previous)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use krkr_engine::{EngineConfig, KrkrEngine};
    use krkr_tjs2::{
        TjsError, TjsErrorKind,
        runtime::{ObjectHandle, Variant},
    };

    use super::{
        MENU_ITEM_BITMAP_CONSTANTS, SYSTEM_METRICS, WINDOW_EX_EVENTS, WINDOW_HIT_TEST_CONSTANTS,
        WINDOW_NOTIFICATIONS, WindowExPlugin,
    };

    fn engine() -> KrkrEngine {
        let mut engine = KrkrEngine::new(EngineConfig::default()).expect("engine");
        engine
            .register_plugin(WindowExPlugin)
            .expect("windowEx plugin");
        engine
    }

    fn class_object(engine: &mut KrkrEngine, name: &str) -> ObjectHandle {
        engine
            .tjs_runtime()
            .global_member(name)
            .object_handle()
            .unwrap_or_else(|| panic!("{name} is not registered"))
    }

    fn eval(engine: &mut KrkrEngine, source: &str) -> Variant {
        engine
            .execute_expression("probe.tjs", source)
            .unwrap_or_else(|error| panic!("{source} failed: {}", error.message))
    }

    fn text(engine: &mut KrkrEngine, source: &str) -> String {
        eval(engine, source)
            .to_tjs_string()
            .unwrap_or_else(|error| panic!("{source} did not produce a string: {}", error.message))
    }

    /// Runs a probe that needs statements: its last line is `return <value>;`
    /// and that value is what the test reads. A script returns void without an
    /// explicit `return`, which is why probes that build state use this instead
    /// of [`text`].
    fn script(engine: &mut KrkrEngine, source: &str) -> Variant {
        engine
            .execute_script("probe.tjs", source)
            .unwrap_or_else(|error| panic!("{source} failed: {}", error.message))
    }

    fn script_text(engine: &mut KrkrEngine, source: &str) -> String {
        script(engine, source)
            .to_tjs_string()
            .unwrap_or_else(|error| panic!("{source} did not produce a string: {}", error.message))
    }

    fn eval_error(engine: &mut KrkrEngine, source: &str) -> TjsError {
        engine
            .execute_expression("probe.tjs", source)
            .expect_err(&format!("{source} should have failed"))
    }

    /// Every member the reference attaches to `Window` (`main.cpp:1011-1037`).
    const WINDOW_MEMBERS: &[&str] = &[
        "minimize",
        "maximize",
        "maximizeBox",
        "minimizeBox",
        "maximized",
        "minimized",
        "showRestore",
        "resetWindowIcon",
        "setWindowIcon",
        "getWindowRect",
        "getClientRect",
        "getNormalRect",
        "disableResize",
        "disableMove",
        "setOverlayBitmap",
        "exSystemMenu",
        "resetExSystemMenu",
        "enableNCMouseEvent",
        "ncHitTest",
        "focusMenuByKey",
        "setMessageHook",
        "bringTo",
        "sendToBack",
        "registerExEvent",
        "getNotificationNum",
        "getNotificationName",
    ];

    const MENU_ITEM_MEMBERS: &[&str] = &[
        "rightJustify",
        "bmpItem",
        "bmpChecked",
        "bmpUnchecked",
        "popupEx",
    ];

    const SYSTEM_MEMBERS: &[&str] = &[
        "getDisplayMonitors",
        "getMonitorInfo",
        "getCursorPos",
        "setCursorPos",
        "getSystemMetrics",
        "readEnvValue",
        "expandEnvString",
        "setApplicationIcon",
        "setIconicPreview",
        "getDoubleClickTime",
        "breathe",
        "isBreathing",
        "clearGraphicCache",
        "getAboutString",
        "getCPUType",
    ];

    #[test]
    fn window_surface_matches_the_reference_member_list() {
        let mut engine = engine();
        let window = class_object(&mut engine, "Window");
        for name in WINDOW_MEMBERS {
            assert!(
                engine.tjs_runtime().has_object_member(window, name),
                "Window.{name} is not registered"
            );
        }
        for (name, _) in WINDOW_HIT_TEST_CONSTANTS {
            assert!(engine.tjs_runtime().has_object_member(window, name));
        }

        let menu_item = class_object(&mut engine, "MenuItem");
        for name in MENU_ITEM_MEMBERS {
            assert!(
                engine.tjs_runtime().has_object_member(menu_item, name),
                "MenuItem.{name} is not registered"
            );
        }
        for (name, _) in MENU_ITEM_BITMAP_CONSTANTS {
            assert!(engine.tjs_runtime().has_object_member(menu_item, name));
        }

        let system = class_object(&mut engine, "System");
        for name in SYSTEM_MEMBERS {
            assert!(
                engine.tjs_runtime().has_object_member(system, name),
                "System.{name} is not registered"
            );
        }

        let scripts = class_object(&mut engine, "Scripts");
        assert!(engine.tjs_runtime().has_object_member(scripts, "setEvalErrorLog"));
    }

    /// The reference attaches `Pad.registerExEvent` and the seven
    /// `Debug.console` functions only when those objects exist
    /// (`ncbind.hpp:2324-2345` returns without registering for a missing
    /// attach target). This engine has neither, so nothing may pretend they are
    /// there — the shape-only stub used to inject dummy objects for them.
    #[test]
    fn pad_and_console_are_not_attached_when_the_engine_has_no_such_objects() {
        let mut engine = engine();
        assert_eq!(engine.tjs_runtime().global_member("Pad"), Variant::Void);
        let debug = class_object(&mut engine, "Debug");
        assert!(!engine.tjs_runtime().has_object_member(debug, "console"));
    }

    /// `window_ex.rs` used to register twelve members that exist in neither
    /// reference tree (M30: "12 members registered that exist in neither
    /// reference tree", commit `ad77af6`). Those are gone.
    #[test]
    fn invented_members_are_gone() {
        let mut engine = engine();
        let window = class_object(&mut engine, "Window");
        let system = class_object(&mut engine, "System");
        for name in [
            "setWindowCornerPreference",
            "setClientRect",
            "registerHotKey",
            "acquireImeControl",
            "resetImeContext",
            "registerDeviceChange",
        ] {
            assert!(
                !engine.tjs_runtime().has_object_member(window, name),
                "Window.{name} is not in the reference and must not be registered"
            );
        }
        for name in [
            "setClipCursor",
            "setDpiAwareness",
            "findWindowEx",
            "loadCursor",
            "classLongPtr",
            "mapVirtualKey",
        ] {
            assert!(
                !engine.tjs_runtime().has_object_member(system, name),
                "System.{name} is not in the reference and must not be registered"
            );
        }
        // A call reaches the engine's own member lookup, not a placeholder.
        let error = eval_error(&mut engine, "Window.setWindowCornerPreference(1)");
        assert_eq!(error.kind, TjsErrorKind::MemberNotFound);
    }

    #[test]
    fn hit_test_constants_match_the_reference() {
        let mut engine = engine();
        for (name, value) in WINDOW_HIT_TEST_CONSTANTS {
            assert_eq!(
                text(&mut engine, &format!("Window.{name}")),
                value.to_string(),
                "Window.{name}"
            );
        }
    }

    #[test]
    fn menu_item_bitmap_constants_match_the_reference() {
        let mut engine = engine();
        for (name, value) in MENU_ITEM_BITMAP_CONSTANTS {
            assert_eq!(
                text(&mut engine, &format!("MenuItem.{name}")),
                value.to_string(),
                "MenuItem.{name}"
            );
        }
    }

    #[test]
    fn notification_table_matches_the_reference() {
        let mut engine = engine();
        assert_eq!(text(&mut engine, "Window.getNotificationNum(\"SIZE\")"), "5");
        assert_eq!(text(&mut engine, "Window.getNotificationName(5)"), "SIZE");
        assert_eq!(
            text(&mut engine, "Window.getNotificationName(0x0112)"),
            "SYSCOMMAND"
        );
        assert_eq!(
            text(&mut engine, "Window.getNotificationNum(\"WININICHANGE\")"),
            "26"
        );
        // The duplicate `WM_SETTINGCHANGE` spelling is written last, so the
        // reverse mapping answers it, exactly like the reference dictionary.
        assert_eq!(
            text(&mut engine, "Window.getNotificationName(0x001A)"),
            "SETTINGCHANGE"
        );
        // An unknown name is -1 and an unknown number is the empty string.
        assert_eq!(text(&mut engine, "Window.getNotificationNum(\"NOPE\")"), "-1");
        assert_eq!(text(&mut engine, "Window.getNotificationName(999999)"), "");
        // The lookups materialize the table on `Window` itself.
        assert_eq!(
            text(
                &mut engine,
                "Window._Notifications === void ? \"missing\" : \"present\""
            ),
            "present"
        );

        // Every entry resolves in both directions. A reverse lookup answers the
        // last spelling written for a number, because later duplicates
        // overwrite the reference dictionary's number -> name entry.
        for (name, value) in WINDOW_NOTIFICATIONS {
            let reverse = WINDOW_NOTIFICATIONS
                .iter()
                .rev()
                .find(|(_, candidate)| candidate == value)
                .map(|(candidate, _)| *candidate)
                .unwrap_or_default();
            assert_eq!(
                text(&mut engine, &format!("Window._Notifications[\"{name}\"]")),
                value.to_string(),
                "{name}"
            );
            assert_eq!(
                text(&mut engine, &format!("Window.getNotificationNum(\"{name}\")")),
                value.to_string(),
                "{name}"
            );
            assert_eq!(
                text(&mut engine, &format!("Window._Notifications[{value}]")),
                reverse,
                "{name}"
            );
            assert_eq!(
                text(&mut engine, &format!("Window.getNotificationName({value})")),
                reverse,
                "{name}"
            );
        }
    }

    /// A script that replaced `Window._Notifications` owns the lookup
    /// (`main.cpp:735-736` only builds the table when the member is absent).
    #[test]
    fn a_script_owned_notification_table_is_used_as_is() {
        let mut engine = engine();
        // The `"0x10"` entry pins the conversion: `ncbPropAccessor::getIntValue`
        // goes through TJS's `(tjs_int)`, so a hex string is 16 and not Rust's
        // `str::parse` result of 0.
        let source = "Window._Notifications = %[\"SIZE\" => 42, \"MOVE\" => \"0x10\"]; \
                      return \"\" + Window.getNotificationNum(\"SIZE\") + \"/\" \
                          + Window.getNotificationNum(\"MOVE\") + \"/\" \
                          + Window.getNotificationNum(\"NOPE\");";
        assert_eq!(script_text(&mut engine, source), "42/16/-1");
    }

    #[test]
    fn notification_lookups_reject_a_missing_argument() {
        let mut engine = engine();
        assert_eq!(
            eval_error(&mut engine, "Window.getNotificationNum()").kind,
            TjsErrorKind::BadParamCount
        );
        assert_eq!(
            eval_error(&mut engine, "Window.getNotificationName()").kind,
            TjsErrorKind::BadParamCount
        );
    }

    #[test]
    fn per_instance_flags_start_false_and_stay_per_instance() {
        let mut engine = engine();
        let source = "\
            var a = new Window(); var b = new Window(); \
            var before = \"\" + a.disableResize + b.disableMove + a.enableNCMouseEvent; \
            a.disableResize = 1; b.disableMove = 1; a.enableNCMouseEvent = true; \
            return before + \"|\" + a.disableResize + b.disableResize + a.disableMove \
                + b.disableMove + a.enableNCMouseEvent + b.enableNCMouseEvent;";
        assert_eq!(script_text(&mut engine, source), "000|100110");
    }

    #[test]
    fn ex_system_menu_keeps_the_script_object() {
        let mut engine = engine();
        let source = "\
            var w = new Window(); \
            var unset = (w.exSystemMenu === void) ? \"void\" : \"set\"; \
            var menu = %[\"caption\" => \"probe\"]; \
            w.exSystemMenu = menu; \
            return unset + \"/\" + (w.exSystemMenu === menu ? \"same\" : \"other\");";
        assert_eq!(script_text(&mut engine, source), "void/same");
    }

    #[test]
    fn message_hooks_follow_the_reference_range_and_return_value() {
        let mut engine = engine();
        let source = "\
            var w = new Window(); var r = [ \
                w.setMessageHook(true), w.setMessageHook(false), \
                w.setMessageHook(true, \"SIZE\"), w.setMessageHook(false, \"SIZE\"), \
                w.setMessageHook(true, 5), w.setMessageHook(false, 5) ]; \
            return \"\" + r[0] + r[1] + r[2] + r[3] + r[4] + r[5];";
        assert_eq!(script_text(&mut engine, source), "101010");

        // An unknown name is `-1` and out of range, like the reference's
        // `getWindowNotificationNum` default.
        assert_eq!(
            eval_error(&mut engine, "(new Window()).setMessageHook(true, \"NOPE\")").message,
            "Unknown failure : FFFFFFFF"
        );
        assert_eq!(
            eval_error(&mut engine, "(new Window()).setMessageHook(true, 0x400)").message,
            "Unknown failure : FFFFFFFF"
        );
    }

    #[test]
    fn window_rects_come_from_the_engine_window_model() {
        let mut engine = engine();
        let source = "\
            var w = new Window(); w.setPos(10, 20); w.setSize(640, 480); \
            var r = w.getWindowRect(); var c = w.getClientRect(); var n = w.getNormalRect(); \
            return \"\" + r.x + \",\" + r.y + \",\" + r.w + \",\" + r.h \
                + \"|\" + c.x + \",\" + c.y + \",\" + c.w + \",\" + c.h \
                + \"|\" + n.w + \",\" + n.h;";
        assert_eq!(
            script_text(&mut engine, source),
            "10,20,640,480|10,20,640,480|640,480"
        );
    }

    #[test]
    fn a_receiver_without_a_window_has_no_rect() {
        let mut engine = engine();
        let source = "\
            var onClass = (Window.getWindowRect() === void) ? \"void\" : \"rect\"; \
            var foreign = %[]; foreign.getWindowRect = Window.getWindowRect; \
            var onForeign = (foreign.getWindowRect() === void) ? \"void\" : \"rect\"; \
            return onClass + \"/\" + onForeign;";
        assert_eq!(script_text(&mut engine, source), "void/void");
    }

    #[test]
    fn nc_hit_test_needs_two_arguments() {
        let mut engine = engine();
        assert_eq!(
            eval_error(&mut engine, "(new Window()).ncHitTest(1)").kind,
            TjsErrorKind::BadParamCount
        );
        // Nothing in this engine writes `Window.HWND`, so the reference's
        // `SendMessage` reaches no window: the answer is 0, never an `HT*` code
        // (`main.cpp:337`).
        assert_eq!(
            text(&mut engine, "(new Window()).ncHitTest(10, 20)"),
            "0"
        );
    }

    #[test]
    fn overlay_bitmap_requires_a_layer() {
        let mut engine = engine();
        // A void argument is the documented "hide" form.
        assert_eq!(
            script(
                &mut engine,
                "var w = new Window(); return w.setOverlayBitmap();"
            ),
            Variant::Void
        );
        assert_eq!(
            eval_error(&mut engine, "(new Window()).setOverlayBitmap(%[\"x\" => 1])").message,
            "Unknown failure : FFFFFFFF"
        );
        // A real `Layer` is accepted; the overlay window it asks for is the
        // part that needs engine support, and the reference's own
        // success path is `TJS_S_OK`.
        assert_eq!(
            eval(&mut engine, "(new Window()).setOverlayBitmap(new Layer())"),
            Variant::Void
        );
    }

    #[test]
    fn icon_members_resolve_their_storage_argument() {
        let mut engine = engine();
        // No storage is mounted, so any non-empty name fails the same way
        // `TVPGetPlacedPath` does.
        assert!(
            eval_error(&mut engine, "Window.setWindowIcon(\"missing.ico\")")
                .message
                .contains("file not found.")
        );
        assert!(
            eval_error(&mut engine, "System.setApplicationIcon(\"missing.ico\")")
                .message
                .contains("file not found.")
        );
        // An empty name is the documented "reset" form and does not look
        // anything up.
        assert_eq!(eval(&mut engine, "Window.setWindowIcon(\"\")"), Variant::Void);
        assert_eq!(eval(&mut engine, "System.setApplicationIcon()"), Variant::Void);
    }

    /// `_loadExternalIcon` (`main.cpp:100-111`, the krkr2 line) resolves the
    /// name with `TVPGetPlacedPath` and then asks for a *locally accessible*
    /// one: an XP3 member or a memory-backed resource resolves but cannot be
    /// opened as a file, which is the reference's `cannot get in archive icon.`
    /// A local file passes the lookup and only the missing platform icon loader
    /// stops it (which the handler reports as a gap, not an error).
    #[test]
    fn icon_storage_rule_asks_for_a_locally_accessible_name() {
        use krkr_assets::ProjectStorage;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "kirakira-windowex-icons-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create project root");
        std::fs::write(root.join("icon.ico"), b"not an icon").expect("write icon");

        let storage = ProjectStorage::for_root(&root).expect("storage");
        let mut engine = KrkrEngine::new(EngineConfig {
            project_storage: Some(std::sync::Arc::new(storage)),
            ..EngineConfig::default()
        })
        .expect("engine");
        engine
            .register_plugin(WindowExPlugin)
            .expect("windowEx plugin");
        // A mounted resource resolves by name but has no local path, exactly
        // like an XP3 member: `KrkrHost::placed_path` is `None` for both
        // (`krkr-assets/src/storage.rs:876-881`).
        engine
            .host_mut()
            .mount_virtual_resource("mounted.icon", b"icon".to_vec())
            .expect("mount");

        assert!(
            eval_error(&mut engine, "Window.setWindowIcon(\"mounted.icon\")")
                .message
                .contains("cannot get in archive icon.")
        );
        assert!(
            eval_error(&mut engine, "System.setApplicationIcon(\"mounted.icon\")")
                .message
                .contains("cannot get in archive icon.")
        );
        // The file exists on disk, so the lookup succeeds.
        assert_eq!(
            eval(&mut engine, "Window.setWindowIcon(\"icon.ico\")"),
            Variant::Void
        );
        assert!(
            eval_error(&mut engine, "Window.setWindowIcon(\"absent.ico\")")
                .message
                .contains("file not found.")
        );

        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn menu_item_properties_follow_the_reference() {
        let mut engine = engine();
        let source = "\
            var w = new Window(); var item = new MenuItem(w, \"probe\"); \
            var before = item.rightJustify; \
            item.rightJustify = 1; \
            var after = item.rightJustify; \
            var unset = item.bmpChecked; \
            item.bmpChecked = 3; \
            var set = item.bmpChecked; \
            item.bmpItem = new Layer(); \
            var layer = item.bmpItem; \
            item.bmpUnchecked = \"0\"; \
            var stringy = item.bmpUnchecked; \
            item.bmpUnchecked = \"0x10\"; \
            var hex = item.bmpUnchecked; \
            item.bmpChecked = \"5abc\"; \
            var prefixed = item.bmpChecked; \
            return \"\" + before + after + unset + set + layer + stringy \
                + \"/\" + hex + \"/\" + prefixed;";
        // `setBmpSelect` stores `(HBITMAP)v.AsInteger()` (`main.cpp:1150`), so a
        // hex string is 16 and a trailing-garbage string is 5 — TJS's
        // conversion, not Rust's `str::parse`.
        assert_eq!(script_text(&mut engine, source), "0103-10/16/5");

        assert!(
            eval_error(&mut engine, "(new MenuItem(new Window(), \"p\")).bmpItem = %[]")
                .message
                .contains("no layer object.")
        );
    }

    #[test]
    fn popup_ex_reports_no_menu() {
        let mut engine = engine();
        // The reference clears the result when no menu could be built, which
        // is what happens here.
        assert_eq!(
            eval(&mut engine, "(new MenuItem(new Window(), \"p\")).popupEx(0)"),
            Variant::Void
        );
    }

    #[test]
    fn system_metrics_validation_and_table() {
        let mut engine = engine();
        // The engine publishes a 1920x1080 screen and desktop by default.
        assert_eq!(
            text(&mut engine, "System.getSystemMetrics(\"CXSCREEN\")"),
            "1920"
        );
        assert_eq!(
            text(&mut engine, "System.getSystemMetrics(\"cyscreen\")"),
            "1080"
        );
        assert_eq!(
            text(&mut engine, "System.getSystemMetrics(\"CXVIRTUALSCREEN\")"),
            "1920"
        );
        // The table is built on first use and carries the Win32 indices.
        assert_eq!(text(&mut engine, "System.metrics.CXCURSOR"), "13");
        assert_eq!(text(&mut engine, "System.metrics.REMOTECONTROL"), "8193");
        // A metric with no host source answers 0.
        assert_eq!(text(&mut engine, "System.getSystemMetrics(\"CXCURSOR\")"), "0");

        assert_eq!(
            eval_error(&mut engine, "System.getSystemMetrics()").kind,
            TjsErrorKind::BadParamCount
        );
        for source in [
            "System.getSystemMetrics(5)",
            "System.getSystemMetrics(\"\")",
            "System.getSystemMetrics(\"NOPE\")",
        ] {
            assert_eq!(eval_error(&mut engine, source).kind, TjsErrorKind::InvalidParam);
        }
    }

    #[test]
    fn monitor_and_cursor_members_report_the_missing_host_sources() {
        let mut engine = engine();
        let source = "\
            var monitors = System.getDisplayMonitors(); \
            var ranged = System.getDisplayMonitors(0, 0, 100, 100); \
            var info = (System.getMonitorInfo() === void) ? \"void\" : \"dict\"; \
            var nearWindow = new Window(); \
            var byWindow = (System.getMonitorInfo(true, nearWindow) === void) ? \"void\" : \"dict\"; \
            var cursor = (System.getCursorPos() === void) ? \"void\" : \"dict\"; \
            return \"\" + monitors.length + \"/\" + ranged.length + \"/\" + info + \"/\" \
                + byWindow + \"/\" + cursor + \"/\" + System.setCursorPos(1, 2);";
        assert_eq!(script_text(&mut engine, source), "0/0/void/void/void/0");

        assert_eq!(
            eval_error(&mut engine, "System.getDisplayMonitors(1, 2, 3)").kind,
            TjsErrorKind::BadParamCount
        );
        // The four-parameter form converts its rectangle the way `GetDictRect`
        // does (`main.cpp:1772-1779`), and an object argument cannot convert.
        assert_eq!(
            eval_error(&mut engine, "System.getDisplayMonitors(1, 2, 3, null)").kind,
            TjsErrorKind::Runtime
        );
        assert_eq!(
            eval_error(&mut engine, "System.getMonitorInfo(true, 5)").kind,
            TjsErrorKind::InvalidParam
        );
        assert_eq!(
            eval_error(&mut engine, "System.getMonitorInfo(1)").kind,
            TjsErrorKind::BadParamCount
        );
        assert_eq!(
            eval_error(&mut engine, "System.setCursorPos(1)").kind,
            TjsErrorKind::BadParamCount
        );
    }

    #[test]
    fn environment_members_read_the_process_environment() {
        // SAFETY: the name is unique to this test, so no other test in this
        // process observes the change.
        unsafe { std::env::set_var("KIRAKIRA_WINDOWEX_PROBE", "probe-value") };
        let mut engine = engine();
        let source = "\
            var value = System.readEnvValue(\"KIRAKIRA_WINDOWEX_PROBE\"); \
            var absent = (System.readEnvValue(\"KIRAKIRA_WINDOWEX_ABSENT\") === void) ? \"void\" : \"set\"; \
            var expanded = System.expandEnvString(\"a%KIRAKIRA_WINDOWEX_PROBE%b\"); \
            var kept = System.expandEnvString(\"%KIRAKIRA_WINDOWEX_ABSENT%\"); \
            return value + \"/\" + absent + \"/\" + expanded + \"/\" + kept;";
        assert_eq!(
            script_text(&mut engine, source),
            "probe-value/void/aprobe-valueb/%KIRAKIRA_WINDOWEX_ABSENT%"
        );

        assert_eq!(
            eval_error(&mut engine, "System.readEnvValue()").kind,
            TjsErrorKind::BadParamCount
        );
        assert_eq!(
            eval_error(&mut engine, "System.readEnvValue(5)").kind,
            TjsErrorKind::InvalidParam
        );
        assert_eq!(
            eval_error(&mut engine, "System.readEnvValue(\"\")").kind,
            TjsErrorKind::InvalidParam
        );
        assert_eq!(
            eval_error(&mut engine, "System.expandEnvString()").kind,
            TjsErrorKind::BadParamCount
        );
    }

    #[test]
    fn members_without_engine_support_keep_the_reference_no_window_path() {
        let mut engine = engine();
        let source = "\
            var w = new Window(); \
            var a = (w.minimize() === void) ? \"void\" : \"value\"; \
            w.maximize(); w.showRestore(); w.focusMenuByKey(0); w.resetWindowIcon(); \
            w.resetExSystemMenu(); w.bringTo(\"top\"); w.sendToBack(); \
            var b = (w.getNormalRect(1) === void) ? \"void\" : \"rect\"; \
            var c = \"\" + w.maximized + w.minimized + w.maximizeBox + w.minimizeBox; \
            w.maximized = 1; w.minimized = true; w.maximizeBox = 0; w.minimizeBox = 0; \
            var d = \"\" + w.maximized + w.minimized + w.maximizeBox + w.minimizeBox; \
            return a + \"/\" + b + \"/\" + c + \"/\" + d;";
        // A window that cannot be maximized reads 0 everywhere, and the
        // setters are the no-ops the reference performs on a null handle.
        assert_eq!(script_text(&mut engine, source), "void/rect/0000/0000");
    }

    #[test]
    fn scripts_eval_error_log_reports_the_previous_value() {
        let mut engine = engine();
        let source = "\
            return \"\" + Scripts.setEvalErrorLog(false) + Scripts.setEvalErrorLog(true) \
                + Scripts.setEvalErrorLog(false);";
        assert_eq!(script_text(&mut engine, source), "101");
        assert_eq!(
            eval_error(&mut engine, "Scripts.setEvalErrorLog()").kind,
            TjsErrorKind::BadParamCount
        );
    }

    #[test]
    fn register_ex_event_caches_the_deferred_names() {
        let mut engine = engine();
        // The call itself must not fail; the caches it fills are the
        // reference's `checkExEvents` state (`main.cpp:662-667`).
        assert_eq!(
            script(
                &mut engine,
                "var w = new Window(); return w.registerExEvent();"
            ),
            Variant::Void
        );
        assert_eq!(
            script_text(
                &mut engine,
                "return \"\" + System.getDoubleClickTime() + \"/\" + System.isBreathing() \
                    + \"/\" + System.getCPUType() + \"/\" + System.setIconicPreview(true);"
            ),
            "500/0/0/0"
        );
        assert_eq!(script(&mut engine, "return System.breathe();"), Variant::Void);
        assert_eq!(
            script(&mut engine, "return System.clearGraphicCache();"),
            Variant::Void
        );
        assert!(text(&mut engine, "System.getAboutString()").contains("Kirakira"));
        assert_eq!(
            eval_error(&mut engine, "System.setIconicPreview()").kind,
            TjsErrorKind::BadParamCount
        );
    }

    #[test]
    fn tables_are_well_formed() {
        // Names are unique per table; only the reverse mapping of a duplicate
        // number depends on order.
        let mut seen = std::collections::BTreeSet::new();
        for (name, _) in WINDOW_NOTIFICATIONS {
            assert!(seen.insert(*name), "duplicate notification name {name}");
        }
        let mut seen = std::collections::BTreeSet::new();
        for (name, _) in SYSTEM_METRICS {
            assert!(seen.insert(*name), "duplicate metric name {name}");
        }
        let mut seen = std::collections::BTreeSet::new();
        for (name, _) in WINDOW_HIT_TEST_CONSTANTS {
            assert!(seen.insert(*name), "duplicate hit-test name {name}");
        }

        // The four names `registerExEvent` caches are part of the event list,
        // which records the argument shape of every extended event.
        let deferred: Vec<&str> = WINDOW_EX_EVENTS
            .iter()
            .filter(|event| event.deferred)
            .map(|event| event.name)
            .collect();
        assert_eq!(
            deferred,
            vec!["onResizing", "onMoving", "onMove", "onNcMouseMove"]
        );
        let names: Vec<&str> = WINDOW_EX_EVENTS.iter().map(|event| event.name).collect();
        assert_eq!(names.len(), 25);
        assert!(WINDOW_EX_EVENTS.iter().all(|event| event.params <= 4));
        assert!(names.contains(&"onWindowsMessageHook"));
    }
}
