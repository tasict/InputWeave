using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Canvas;
using InputWeave.Overlay;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

/// <summary>Hotkeys as Windows shows them, and recording new ones.</summary>
public class ShortcutTextTests
{
    private static readonly HeldModifiers None = default;
    private static readonly HeldModifiers Ctrl = new(Control: true, Alt: false, Shift: false, Windows: false);
    private static readonly HeldModifiers CtrlAlt = new(Control: true, Alt: true, Shift: false, Windows: false);
    private static readonly HeldModifiers All = new(Control: true, Alt: true, Shift: true, Windows: true);

    [Fact]
    public void SpellsHotkeysOut()
    {
        Assert.Equal("Ctrl + Alt + Shift + Esc", ShortcutText.Text("control+alt+shift+Escape"));
        Assert.Equal("Ctrl + Alt + Shift + L", ShortcutText.Text("control+alt+shift+KeyL"));
        // The service's order of modifiers, whatever the text's; any case.
        Assert.Equal("Shift + Win + 2", ShortcutText.Text("gui+shift+Digit2"));
        Assert.Equal("Ctrl + ArrowLeft", ShortcutText.Text("CONTROL+ArrowLeft"));
        Assert.Equal("Alt + F5", ShortcutText.Text("alt+F5"));
        Assert.Equal("Ctrl + Space", ShortcutText.Text("control+Space"));
        Assert.Equal("Ctrl+Alt+Shift+2", ShortcutText.Compact("control+alt+shift+Digit2"));
        Assert.Equal("", ShortcutText.Text(""));
        Assert.Equal("L", ShortcutText.Key("KeyL"));
        Assert.Equal("9", ShortcutText.Key("Digit9"));
        Assert.Equal("Esc", ShortcutText.Key("Escape"));
        Assert.Equal("Keyboard", ShortcutText.Key("Keyboard"));
        Assert.Equal("Numpad1", ShortcutText.Key("Numpad1"));
    }

    [Fact]
    public void WritesTheJumps()
    {
        Assert.Equal("Ctrl+Alt+Shift+2", ShortcutText.Jump("control+alt+shift", 2));
        Assert.Equal("Ctrl + Alt + Shift + 2", ShortcutText.Jump("control+alt+shift", 2, compact: false));
        Assert.Equal("Alt+Win+9", ShortcutText.Jump("gui+alt", 9));
        // No number, past the ninth, or the jumps off.
        Assert.Null(ShortcutText.Jump("control+alt+shift", 0));
        Assert.Null(ShortcutText.Jump("control+alt+shift", 10));
        Assert.Null(ShortcutText.Jump("", 2));
        Assert.Equal("Ctrl + Alt + Shift + 1…9", ShortcutText.JumpRange("control+alt+shift"));
        Assert.Equal("", ShortcutText.JumpRange(""));
    }

    [Fact]
    public void ShowsEachRow()
    {
        var hotkeys = Shortcut.Defaults;
        Assert.Equal(new UiText(TextKeys.Verbatim, "Ctrl + Alt + Shift + Esc"), ShortcutText.Shown(hotkeys, ShortcutRow.TakeBack));
        Assert.Equal(new UiText(TextKeys.Verbatim, "Ctrl + Alt + Shift + L"), ShortcutText.Shown(hotkeys, ShortcutRow.Lock));
        Assert.Equal(new UiText(TextKeys.Verbatim, "Ctrl + Alt + Shift + 1…9"), ShortcutText.Shown(hotkeys, ShortcutRow.Jump));
        hotkeys.JumpModifiers = "";
        Assert.Equal(new UiText(TextKeys.ShortcutOff), ShortcutText.Shown(hotkeys, ShortcutRow.Jump));
        Assert.Equal(TextKeys.ShortcutPressJump, ShortcutText.Prompt(ShortcutRow.Jump));
        Assert.Equal(TextKeys.ShortcutPressSingle, ShortcutText.Prompt(ShortcutRow.Lock));
        Assert.Equal(TextKeys.ShortcutJump, ShortcutText.Title(ShortcutRow.Jump));
    }

    [Fact]
    public void ChangesOneRowOfACopy()
    {
        var before = Shortcut.Defaults;
        var after = ShortcutText.With(before, ShortcutRow.Lock, "control+alt+KeyK");
        Assert.Equal("control+alt+KeyK", after.Lock);
        Assert.Equal(Shortcut.DefaultLock, before.Lock);
        Assert.Equal("", ShortcutText.With(before, ShortcutRow.Jump, "").JumpModifiers);
        Assert.Equal("control+F1", ShortcutText.Get(ShortcutText.With(before, ShortcutRow.TakeBack, "control+F1"), ShortcutRow.TakeBack));
        Assert.Equal(Shortcut.Defaults, Shortcut.Defaults);
    }

    [Fact]
    public void NamesKeysByTheirScanCodes()
    {
        Assert.Equal("KeyL", ShortcutText.KeyName(0x26, extended: false));
        Assert.Equal("Escape", ShortcutText.KeyName(0x01, extended: false));
        Assert.Equal("Digit1", ShortcutText.KeyName(0x02, extended: false));
        // The same scan code is another key with the 0xE0 prefix.
        Assert.Equal("Delete", ShortcutText.KeyName(0x53, extended: true));
        Assert.Equal("NumpadDecimal", ShortcutText.KeyName(0x53, extended: false));
        Assert.Equal("ControlRight", ShortcutText.KeyName(0x1d, extended: true));
        Assert.Equal("ArrowLeft", ShortcutText.KeyName(0x4b, extended: true));
        Assert.Null(ShortcutText.KeyName(0x00, extended: false));
    }

    [Fact]
    public void WritesTheModifiersHeldAsTheServiceDoes()
    {
        Assert.Equal("", None.Names);
        Assert.Equal("control+alt", CtrlAlt.Names);
        Assert.Equal("control+alt+shift+gui", All.Names);
        Assert.Equal("shift+gui", new HeldModifiers(Control: false, Alt: false, Shift: true, Windows: true).Names);
        Assert.False(None.Any);
        Assert.True(Ctrl.Any);
    }

    [Fact]
    public void RecordsAKeyWithItsModifiers()
    {
        Assert.Equal(new Recorded(RecordStep.Set, "control+alt+KeyK"), ShortcutText.Record(ShortcutRow.Lock, "KeyK", CtrlAlt));
        Assert.Equal(new Recorded(RecordStep.Set, "control+alt+shift+gui+F5"), ShortcutText.Record(ShortcutRow.TakeBack, "F5", All));
        // Windows reports the modifiers going down: those wait for the key.
        Assert.Equal(new Recorded(RecordStep.Wait), ShortcutText.Record(ShortcutRow.Lock, "ControlLeft", Ctrl));
        Assert.Equal(new Recorded(RecordStep.Wait), ShortcutText.Record(ShortcutRow.Jump, "AltRight", CtrlAlt));
        Assert.Equal(new Recorded(RecordStep.Wait), ShortcutText.Record(ShortcutRow.TakeBack, "MetaLeft", None));
        // Without a modifier it goes to the service, which says one is needed.
        Assert.Equal(new Recorded(RecordStep.Set, "KeyL"), ShortcutText.Record(ShortcutRow.Lock, "KeyL", None));
        // A key without a name cannot be used.
        Assert.Equal(new Recorded(RecordStep.Unusable), ShortcutText.Record(ShortcutRow.Lock, null, CtrlAlt));
    }

    [Fact]
    public void EscapeCancelsAndDeleteGoesBack()
    {
        Assert.Equal(new Recorded(RecordStep.Cancel), ShortcutText.Record(ShortcutRow.Lock, "Escape", None));
        Assert.Equal(new Recorded(RecordStep.Cancel), ShortcutText.Record(ShortcutRow.Jump, "Escape", None));
        Assert.Equal(new Recorded(RecordStep.Set, Shortcut.DefaultLock), ShortcutText.Record(ShortcutRow.Lock, "Delete", None));
        Assert.Equal(new Recorded(RecordStep.Set, Shortcut.DefaultTakeBack), ShortcutText.Record(ShortcutRow.TakeBack, "Backspace", None));
        Assert.Equal(new Recorded(RecordStep.Set, Shortcut.DefaultJumpModifiers), ShortcutText.Record(ShortcutRow.Jump, "Delete", None));
        // With modifiers they are keys like any other.
        Assert.Equal(new Recorded(RecordStep.Set, "control+alt+shift+Escape"),
            ShortcutText.Record(ShortcutRow.TakeBack, "Escape", new HeldModifiers(true, true, true, false)));
        Assert.Equal(new Recorded(RecordStep.Set, "control+Delete"), ShortcutText.Record(ShortcutRow.Lock, "Delete", Ctrl));
    }

    [Fact]
    public void TheJumpsTakeTheModifiersHeld()
    {
        Assert.Equal(new Recorded(RecordStep.Set, "control+alt"), ShortcutText.Record(ShortcutRow.Jump, "Digit5", CtrlAlt));
        Assert.Equal(new Recorded(RecordStep.Set, "control"), ShortcutText.Record(ShortcutRow.Jump, null, Ctrl));
        // A key alone has no modifiers to take; keep listening.
        Assert.Equal(new Recorded(RecordStep.Wait), ShortcutText.Record(ShortcutRow.Jump, "KeyA", None));
    }

    [Fact]
    public void SaysWhyTheServiceRefused()
    {
        var before = Shortcut.Defaults;
        Assert.Equal(new UiText(TextKeys.ShortcutSingleKeys),
            ShortcutText.Refusal("needs_modifier", ShortcutRow.Lock, ShortcutText.With(before, ShortcutRow.Lock, "KeyL"), before));
        Assert.Equal(new UiText(TextKeys.ShortcutUnknownKey),
            ShortcutText.Refusal("unknown_key", ShortcutRow.Lock, before, before));
        Assert.Equal(new UiText(TextKeys.ShortcutNotChanged),
            ShortcutText.Refusal("other", ShortcutRow.Lock, before, before));
        // Changed but not saved, or no answer in time.
        Assert.Equal(new UiText(TextKeys.ShortcutNotSaved),
            ShortcutText.Refusal("not_saved", ShortcutRow.Lock, before, before));
        Assert.Equal(new UiText(TextKeys.TitleNotResponding),
            ShortcutText.Refusal("timeout", null, before, before));

        // The lock made the same as taking back control.
        Assert.Equal(
            new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutTakeBack)),
            ShortcutText.Refusal("duplicate", ShortcutRow.Lock, ShortcutText.With(before, ShortcutRow.Lock, Shortcut.DefaultTakeBack), before));
        // Taking back control made one of the jumps.
        Assert.Equal(
            new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutJump)),
            ShortcutText.Refusal("duplicate", ShortcutRow.TakeBack, ShortcutText.With(before, ShortcutRow.TakeBack, "control+alt+shift+Digit3"), before));
        // The jumps' modifiers made with a digit what the lock is.
        var lockOnDigit = ShortcutText.With(ShortcutText.With(before, ShortcutRow.Jump, ""), ShortcutRow.Lock, "control+alt+Digit1");
        Assert.Equal(
            new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutLock)),
            ShortcutText.Refusal("duplicate", ShortcutRow.Jump, ShortcutText.With(lockOnDigit, ShortcutRow.Jump, "control+alt"), lockOnDigit));
        // Reset, or no telling which.
        Assert.Equal(new UiText(TextKeys.ShortcutUsedElsewhere), ShortcutText.Refusal("duplicate", null, before, before));
        Assert.Equal(new UiText(TextKeys.ShortcutUsedElsewhere),
            ShortcutText.Refusal("duplicate", ShortcutRow.Lock, ShortcutText.With(before, ShortcutRow.Lock, "control+KeyQ"), before));
    }

    [Fact]
    public void TheRelativeModeRow()
    {
        // A fourth row that can be turned off.
        var before = Shortcut.Defaults;
        Assert.Equal(new UiText(TextKeys.Verbatim, "Ctrl + Alt + Shift + G"), ShortcutText.Shown(before, ShortcutRow.Relative));
        var off = ShortcutText.With(before, ShortcutRow.Relative, "");
        Assert.Equal("", off.Relative);
        Assert.True(off.HasRelative);
        Assert.Equal(new UiText(TextKeys.ShortcutOff), ShortcutText.Shown(off, ShortcutRow.Relative));
        Assert.Equal(TextKeys.ShortcutRelative, ShortcutText.Title(ShortcutRow.Relative));
        Assert.Equal(TextKeys.ShortcutPressKeys, ShortcutText.Prompt(ShortcutRow.Relative));
        Assert.True(ShortcutText.CanTurnOff(ShortcutRow.Relative));
        Assert.True(ShortcutText.CanTurnOff(ShortcutRow.Jump));
        Assert.False(ShortcutText.CanTurnOff(ShortcutRow.Lock));
        Assert.False(ShortcutText.CanTurnOff(ShortcutRow.TakeBack));
        Assert.Equal(new Recorded(RecordStep.Set, Shortcut.DefaultRelative), ShortcutText.Record(ShortcutRow.Relative, "Delete", None));
        Assert.Equal(new Recorded(RecordStep.Set, "control+alt+KeyR"), ShortcutText.Record(ShortcutRow.Relative, "KeyR", CtrlAlt));

        // Reset Shortcuts asks about it only when the service knows it.
        Assert.True(ShortcutText.IsDefault(before, relative: true, doublePress: true));
        Assert.False(ShortcutText.IsDefault(off, relative: true, doublePress: true));
        Assert.True(ShortcutText.IsDefault(off, relative: false, doublePress: true));
        var old = before.Clone();
        old.ClearRelative();
        Assert.True(ShortcutText.IsDefault(old, relative: false, doublePress: true));
        Assert.False(ShortcutText.IsDefault(old, relative: true, doublePress: true));
        Assert.False(ShortcutText.IsDefault(ShortcutText.With(before, ShortcutRow.Lock, "control+KeyK"), relative: false, doublePress: true));
    }

    [Fact]
    public void NamesTheRowsTheRelativeModeHotkeyClashesWith()
    {
        var before = Shortcut.Defaults;
        // The relative mode hotkey made the lock: the lock is named, whichever code.
        var relativeOnLock = ShortcutText.With(before, ShortcutRow.Relative, Shortcut.DefaultLock);
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutLock)),
            ShortcutText.Refusal("duplicate_relative", ShortcutRow.Relative, relativeOnLock, before));
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutLock)),
            ShortcutText.Refusal("duplicate", ShortcutRow.Relative, relativeOnLock, before));
        // The lock made the relative mode hotkey.
        var lockOnRelative = ShortcutText.With(before, ShortcutRow.Lock, Shortcut.DefaultRelative);
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutRelative)),
            ShortcutText.Refusal("duplicate_relative", ShortcutRow.Lock, lockOnRelative, before));
        // The jumps' modifiers made with a digit what the relative mode hotkey is.
        var relativeOnDigit = ShortcutText.With(ShortcutText.With(before, ShortcutRow.Jump, ""), ShortcutRow.Relative, "control+alt+Digit3");
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutRelative)),
            ShortcutText.Refusal("duplicate", ShortcutRow.Jump, ShortcutText.With(relativeOnDigit, ShortcutRow.Jump, "control+alt"), relativeOnDigit));
        // The service's rule: the same key, one's modifiers among the
        // other's. Ctrl + Alt + L fires whenever Ctrl + Alt + Shift + L would.
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutLock)),
            ShortcutText.Refusal("duplicate_relative", ShortcutRow.Relative, ShortcutText.With(before, ShortcutRow.Relative, "control+alt+KeyL"), before));
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutTakeBack)),
            ShortcutText.Refusal("duplicate", ShortcutRow.Lock, ShortcutText.With(before, ShortcutRow.Lock, "control+alt+shift+gui+Escape"), before));
        Assert.True(ShortcutText.Clash("alt+control+KeyL", "control+alt+shift+keyl"));
        Assert.False(ShortcutText.Clash("control+alt+KeyL", "control+shift+KeyL"));
        Assert.False(ShortcutText.Clash("control+alt+KeyL", ""));
        // Turned off, it uses nothing.
        var off = ShortcutText.With(before, ShortcutRow.Relative, "");
        Assert.Equal(new UiText(TextKeys.ShortcutUsedElsewhere),
            ShortcutText.Refusal("duplicate", ShortcutRow.Lock, ShortcutText.With(off, ShortcutRow.Lock, ""), off));
        // A code from the service, with no telling which row.
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutRelative)),
            ShortcutText.Refusal("duplicate_relative", null, before, before));
    }

    /// <summary>Taking control back and the lock may be a single key such as F13; the service says which keys, and the app tells why by row.</summary>
    [Fact]
    public void TakesSingleKeysForTakingBackAndTheLock()
    {
        Assert.Equal("F13", ShortcutText.Text("F13"));
        Assert.Equal("ScrollLock", ShortcutText.Text("ScrollLock"));
        Assert.Equal("Ctrl + F13", ShortcutText.Text("control+F13"));
        Assert.Equal("Pause", ShortcutText.Compact("Pause"));
        // A key alone goes to the service as it is, for every row but the jumps.
        Assert.Equal(new Recorded(RecordStep.Set, "F13"), ShortcutText.Record(ShortcutRow.TakeBack, "F13", None));
        Assert.Equal(new Recorded(RecordStep.Set, "ScrollLock"), ShortcutText.Record(ShortcutRow.Lock, "ScrollLock", None));
        Assert.Equal(new Recorded(RecordStep.Set, "F13"), ShortcutText.Record(ShortcutRow.Relative, "F13", None));
        Assert.Equal(new Recorded(RecordStep.Wait), ShortcutText.Record(ShortcutRow.Jump, "F13", None));
        // Windows names them by scan code.
        Assert.Equal("F13", ShortcutText.KeyName(0x64, extended: false));
        Assert.Equal("F19", ShortcutText.KeyName(0x6A, extended: false));
        Assert.Equal("ScrollLock", ShortcutText.KeyName(0x46, extended: false));
        Assert.Equal("Pause", ShortcutText.KeyName(0x45, extended: false));
        // With Ctrl held, Pause comes as Break, which is Pause too.
        Assert.Equal("Pause", ShortcutText.KeyName(0x46, extended: true));
        Assert.Equal(new Recorded(RecordStep.Set, "control+Pause"), ShortcutText.Record(ShortcutRow.Lock, ShortcutText.KeyName(0x46, extended: true), Ctrl));
        Assert.Equal(TextKeys.ShortcutPressSingle, ShortcutText.Prompt(ShortcutRow.TakeBack));
        Assert.Equal(TextKeys.ShortcutPressSingle, ShortcutText.Prompt(ShortcutRow.Lock));
        Assert.Equal(TextKeys.ShortcutPressKeys, ShortcutText.Prompt(ShortcutRow.Relative));
        Assert.Equal(TextKeys.ShortcutPressJump, ShortcutText.Prompt(ShortcutRow.Jump));

        var before = Shortcut.Defaults;
        foreach (var row in new[] { ShortcutRow.TakeBack, ShortcutRow.Lock })
        {
            Assert.Equal(new UiText(TextKeys.ShortcutSingleKeys),
                ShortcutText.Refusal("needs_modifier", row, ShortcutText.With(before, row, "KeyK"), before));
        }
        Assert.Equal(new UiText(TextKeys.ShortcutNeedsModifier),
            ShortcutText.Refusal("needs_modifier", ShortcutRow.Relative, ShortcutText.With(before, ShortcutRow.Relative, "F13"), before));
        Assert.Equal(new UiText(TextKeys.ShortcutNeedsModifier), ShortcutText.Refusal("needs_modifier", null, before, before));
        // F13 alone fires whatever modifiers are held: it clashes with any hotkey on F13.
        var lockAlone = ShortcutText.With(before, ShortcutRow.TakeBack, "control+F13");
        Assert.Equal(new UiText(TextKeys.ShortcutUsedFor, new UiText(TextKeys.ShortcutTakeBack)),
            ShortcutText.Refusal("duplicate", ShortcutRow.Lock, ShortcutText.With(lockAlone, ShortcutRow.Lock, "F13"), lockAlone));
        Assert.True(ShortcutText.Clash("F13", "control+alt+F13"));
        Assert.False(ShortcutText.Clash("F13", "F14"));
    }

    /// <summary>The switch for pressing Ctrl twice: shown only when the service reports it, part of Reset Shortcuts, and named in the panel.</summary>
    [Fact]
    public void TheDoublePress()
    {
        var unreported = Shortcut.Defaults;
        unreported.ClearTakeBackDouble();
        var old = Sample.Model(active: "mac", hotkeys: unreported);
        Assert.False(old.DoublePressSupported);
        Assert.False(old.DoublePressOn);
        Assert.Equal(new UiText(TextKeys.SubtitleControlling, "Ctrl + Alt + Shift + Esc"), PanelText.Subtitle(old));
        Assert.True(ShortcutText.IsDefault(unreported, relative: true, doublePress: false));
        // Nor does a service that reports no hotkeys at all.
        Assert.False(Sample.Model(active: "mac").DoublePressSupported);

        var off = Sample.Model(active: "mac", hotkeys: Shortcut.Defaults);
        Assert.True(off.DoublePressSupported);
        Assert.False(off.DoublePressOn);
        Assert.Equal(new UiText(TextKeys.SubtitleControlling, "Ctrl + Alt + Shift + Esc"), PanelText.Subtitle(off));
        Assert.True(ShortcutText.IsDefault(Shortcut.Defaults, relative: true, doublePress: true));

        var onKeys = ShortcutText.WithDoublePress(Shortcut.Defaults, true);
        Assert.Equal(Shortcut.DoubleControl, onKeys.TakeBackDouble);
        Assert.Equal("", ShortcutText.WithDoublePress(onKeys, false).TakeBackDouble);
        var on = Sample.Model(active: "mac", hotkeys: onKeys);
        Assert.True(on.DoublePressOn);
        Assert.Equal(new UiText(TextKeys.SubtitleControllingDouble, "Ctrl + Alt + Shift + Esc"), PanelText.Subtitle(on));
        // On, Reset Shortcuts has something to do; the defaults turn it off.
        Assert.False(ShortcutText.IsDefault(onKeys, relative: true, doublePress: true));
        Assert.Equal("", Shortcut.Defaults.TakeBackDouble);
        Assert.True(Shortcut.Defaults.HasTakeBackDouble);
        // In relative mouse with its hotkey off, the shortcut alone: the
        // service ignores the double press then. A single key reads as itself.
        var relativeOff = ShortcutText.With(onKeys, ShortcutRow.Relative, "");
        Assert.Equal(new UiText(TextKeys.SubtitleControlling, "Ctrl + Alt + Shift + Esc"),
            PanelText.Subtitle(Sample.Model(active: "mac", relative: "mac", hotkeys: relativeOff)));
        Assert.Equal(new UiText(TextKeys.SubtitleControllingDouble, "F13"),
            PanelText.Subtitle(Sample.Model(active: "mac", hotkeys: ShortcutText.With(onKeys, ShortcutRow.TakeBack, "F13"))));
    }

    [Fact]
    public void OffersEveryModifierToSwitchWith()
    {
        Assert.Equal(
            [SwitchModifier.None, SwitchModifier.Control, SwitchModifier.Alt, SwitchModifier.Shift, SwitchModifier.Gui],
            ShortcutText.SwitchChoices.Select(c => c.Modifier));
        Assert.Equal(ShortcutText.SwitchChoices.Count, ShortcutText.SwitchChoices.Select(c => c.Key).Distinct().Count());
        // The choice's place is the enum's value, which the ComboBox relies on.
        Assert.All(ShortcutText.SwitchChoices.Select((c, i) => (c, i)), x => Assert.Equal(x.i, (int)x.c.Modifier));
    }
}

/// <summary>What the app makes of the hotkeys, names and switches the service reports.</summary>
public class DeviceSharingTests
{
    private static AgentSnapshot WithMac(Action<DeviceStatus> change)
    {
        var model = Sample.Model();
        var status = model.Status.Clone();
        change(status.Devices.Single(d => d.Id == "mac"));
        return model with { Status = status };
    }

    [Fact]
    public void UsesTheDefaultHotkeysUntilTheServiceReportsSome()
    {
        var model = Sample.Model();
        Assert.Equal(Shortcut.Defaults, model.Hotkeys);
        Assert.Equal("Ctrl + Alt + Shift + Esc", model.TakeBackShortcut);
        Assert.Equal("Ctrl + Alt + Shift + L", model.LockShortcut);
        Assert.Equal("Ctrl+Alt+Shift+3", model.JumpShortcut(3));

        var reported = Sample.Model(hotkeys: new Hotkeys { TakeBack = "control+F12", Lock = "alt+KeyL", JumpModifiers = "" });
        Assert.Equal("Ctrl + F12", reported.TakeBackShortcut);
        Assert.Equal("Alt + L", reported.LockShortcut);
        Assert.Null(reported.JumpShortcut(3));
        // Relative mode only as the service says; one from before says nothing.
        var old = Sample.Model(relativeSupported: false);
        Assert.False(old.RelativeSupported);
        Assert.Null(old.RelativeShortcut);
        Assert.Equal("Ctrl + Alt + Shift + G", model.RelativeShortcut);
        var relative = Sample.Model(hotkeys: new Hotkeys { TakeBack = "control+F12", Lock = "alt+KeyL", Relative = "control+alt+KeyG" });
        Assert.True(relative.RelativeSupported);
        Assert.Equal("Ctrl + Alt + G", relative.RelativeShortcut);
        var off = Sample.Model(hotkeys: new Hotkeys { TakeBack = "control+F12", Lock = "alt+KeyL", Relative = "" });
        Assert.True(off.RelativeSupported);
        Assert.Null(off.RelativeShortcut);
    }

    [Fact]
    public void SendsFilesOnlyWhereTheyAreOn()
    {
        Assert.True(Sample.Model().CanReceiveFiles("mac"));
        var off = Sample.Model(macFilesOff: true);
        Assert.True(off.FilesOff("mac"));
        Assert.False(off.CanReceiveFiles("mac"));
        Assert.Empty(off.FileReceivers);
        Assert.Single(off.ConnectedDevices);
        Assert.False(Sample.Model(macOnline: false).CanReceiveFiles("mac"));
        Assert.False(Sample.Model().CanReceiveFiles("pc"));
        Assert.False(Sample.Model().FilesOff("ghost"));
    }

    [Fact]
    public void SaysWhyAComputerTakesNoFiles()
    {
        Assert.Null(DeviceText.CannotReceive(Sample.Model(), "pc", compact: false));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), DeviceText.CannotReceive(Sample.Model(macOnline: false), "mac", compact: false));
        Assert.Equal(
            new UiText(TextKeys.DeviceFilesOff, "MacBook Pro"),
            DeviceText.CannotReceive(Sample.Model(macFilesOff: true), "mac", compact: false));
        Assert.Equal(new UiText(TextKeys.DeviceFilesOffShort), DeviceText.CannotReceive(Sample.Model(macFilesOff: true), "mac", compact: true));
    }

    [Fact]
    public void KnowsWhenAComputerWasRenamedHere()
    {
        Assert.Null(Sample.Model().ReportedName("mac"));
        Assert.Null(WithMac(d => d.ReportedName = "MacBook Pro").ReportedName("mac"));
        var renamed = WithMac(d =>
        {
            d.ReportedName = "MacBook Pro";
            d.Name = "Desk Mac";
        });
        Assert.Equal("MacBook Pro", renamed.ReportedName("mac"));
        Assert.Equal("Desk Mac", renamed.NameOf("mac"));
        Assert.Null(renamed.ReportedName("ghost"));
    }
}

public class NameBesideTests
{
    [Fact]
    public void ShowsTheShortcutWhileTheNameKeepsRoom()
    {
        // 200 of room: the shortcut 90 wide and a gap of 4 leave 106 for the name.
        Assert.Equal((true, 106.0), CanvasGeometry.NameBeside(200, 90, 4, 24));
        // Measured widths are rounded up, as text is laid out.
        Assert.Equal((true, 105.0), CanvasGeometry.NameBeside(200, 90.2, 4, 24));
        // Too little left: the name takes it all.
        Assert.Equal((false, 100.0), CanvasGeometry.NameBeside(100, 90, 4, 24));
        Assert.Equal((true, 24.0), CanvasGeometry.NameBeside(118, 90, 4, 24));
        // No shortcut, or no room at all.
        Assert.Equal((false, 200.0), CanvasGeometry.NameBeside(200, 0, 4, 24));
        Assert.Equal((false, 0.0), CanvasGeometry.NameBeside(-5, 90, 4, 24));
    }
}

public class ScreenIdentityTests
{
    private static Screen Arranged(string id, double width, double height, double scale) =>
        new() { Id = id, Width = width, Height = height, Scale = scale };

    [Fact]
    public void NumbersMonitorsAsTheArrangementListsThem()
    {
        var arranged = new[] { Arranged(@"\\.\DISPLAY2", 1920, 1080, 1), Arranged(@"\\.\DISPLAY1", 2560, 1440, 1.5) };
        var monitors = new[]
        {
            new MonitorPixels(@"\\.\DISPLAY1", 0, 0, 3840, 2160, 1.5, true),
            new MonitorPixels(@"\\.\display2", 3840, 0, 5760, 1080, 1, false),
        };
        var labels = ScreenIdentity.Labels(arranged, monitors);
        Assert.Equal([(2, "3840 × 2160"), (1, "1920 × 1080")], labels.Select(l => (l.Number, l.Size)));
        Assert.Equal(monitors, labels.Select(l => l.Monitor));
    }

    [Fact]
    public void WithoutAnArrangementTheMainMonitorComesFirst()
    {
        var monitors = new[]
        {
            new MonitorPixels(@"\\.\DISPLAY3", 2560, 0, 4480, 1080, 1, false),
            new MonitorPixels(@"\\.\DISPLAY1", 0, 0, 2560, 1440, 1, true),
            new MonitorPixels(@"\\.\DISPLAY2", -1920, 0, 0, 1080, 1, false),
        };
        var labels = ScreenIdentity.Labels([], monitors);
        Assert.Equal([(3, "1920 × 1080"), (1, "2560 × 1440"), (2, "1920 × 1080")], labels.Select(l => (l.Number, l.Size)));
    }

    [Fact]
    public void StaysAsLongAsTheServiceSays()
    {
        Assert.Equal(TimeSpan.FromMilliseconds(2500), ScreenIdentity.ShownFor(0));
        Assert.Equal(TimeSpan.FromSeconds(4), ScreenIdentity.ShownFor(4000));
    }

    [Fact]
    public void WritesThisComputersJump()
    {
        var model = Sample.Model();
        var arrangement = new Arrangement();
        arrangement.Devices.Add(new ArrangedDevice { Id = "pc", ThisDevice = true, JumpSlot = 2 });
        Assert.Equal("Ctrl + Alt + Shift + 2", ScreenIdentity.Jump(model with { Arrangement = arrangement }));
        arrangement.Devices[0].JumpSlot = 0;
        Assert.Null(ScreenIdentity.Jump(model with { Arrangement = arrangement }));
        Assert.Null(ScreenIdentity.Jump(model));
    }

    [Fact]
    public void ArgumentsShowTheNamesAndSampleComputers()
    {
        var identify = Launch.Parse([Launch.ShowIdentifyArgument]);
        Assert.True(identify.Identify);
        Assert.Null(identify.IdentifyFor);
        Assert.True(identify.Testing);
        Assert.Equal(20.0, Launch.Parse([Launch.ShowIdentifyArgument, "20"]).IdentifyFor);
        Assert.Null(Launch.Parse([Launch.ShowIdentifyArgument, "soon"]).IdentifyFor);
        // A sample glow: the edge, and for how long it is held.
        var glow = Launch.Parse([Launch.ShowGlowArgument, "left", "15"]);
        Assert.Equal("left", glow.Glow);
        Assert.Equal(15.0, glow.GlowFor);
        Assert.True(glow.Testing);
        Assert.Equal("right", Launch.Parse([Launch.ShowGlowArgument]).Glow);
        Assert.Null(Launch.Parse([Launch.ShowGlowArgument, "top"]).GlowFor);
        Assert.Null(Launch.Parse([]).Glow);
        Assert.True(Launch.Parse([Launch.ShowSampleDevicesArgument]).SampleDevices);
        Assert.True(Launch.Parse([Launch.ShowSampleDevicesArgument]).Testing);
        Assert.False(Launch.Parse([]).Identify);
        Assert.False(Launch.Parse([]).SampleDevices);
    }
}
