using Inputweave.App.V1;
using InputWeave.Panel;

namespace InputWeave.Pairing;

/// <summary>
/// What to tell the user about a pairing that did not work,
/// as on the Mac.
/// </summary>
public static class PairingText
{
    public static UiText Failure(PairingState state)
    {
        object name = state.PeerName.Length == 0 ? new UiText(TextKeys.PairingTheOtherComputer) : state.PeerName;
        return state.Error switch
        {
            "too_many_attempts" => new(TextKeys.PairingTooManyAttempts),
            "expired" => new(TextKeys.PairingExpired),
            "rejected" => new(TextKeys.PairingRejected, name),
            "not_allowed" => new(TextKeys.PairingNotAllowed, name),
            "busy" => new(TextKeys.PairingBusy, name),
            "id_conflict" => new(TextKeys.PairingIdConflict, name),
            "incompatible" => new(TextKeys.PairingIncompatible, name),
            "unreachable" => new(TextKeys.PairingUnreachable, name),
            "cancelled" => new(TextKeys.PairingCancelled),
            _ => new(TextKeys.PairingDidNotWork),
        };
    }

    /// <summary>Why a pairing could not start, from the service's refusal code.</summary>
    public static UiText StartFailure(string code) => code switch
    {
        "busy" => new(TextKeys.PairingStartBusy),
        "invalid_address" => new(TextKeys.PairingStartInvalidAddress),
        "unreachable" or "unknown_device" => new(TextKeys.PairingStartUnreachable),
        _ => new(TextKeys.PairingStartFailed),
    };
}

/// <summary>The six digits of a pairing code, as typed and as shown.</summary>
public static class PairingCode
{
    public const int Length = 6;

    /// <summary>What a typed code keeps: digits only, at most six.</summary>
    public static string Clean(string typed) =>
        new(typed.Where(char.IsAsciiDigit).Take(Length).ToArray());

    /// <summary>"382 914": two groups of three, as on screen.</summary>
    public static string Grouped(string code) =>
        code.Length == Length ? $"{code[..3]} {code[3..]}" : code;

    /// <summary>"3 8 2 9 1 4": what a screen reader says, digit by digit.</summary>
    public static string Spoken(string code) => string.Join(' ', code.ToCharArray());

    /// <summary>"1:58": what is left until a Unix time in milliseconds, never negative.</summary>
    public static string Countdown(ulong expiresAtUnixMs, DateTimeOffset now)
    {
        var deadline = DateTimeOffset.FromUnixTimeMilliseconds((long)Math.Min(expiresAtUnixMs, (ulong)long.MaxValue / 2));
        var seconds = Math.Max(0, (long)Math.Ceiling((deadline - now).TotalSeconds));
        return $"{seconds / 60}:{seconds % 60:00}";
    }

    /// <summary>
    /// Whether this computer shows a code for another one to type: the
    /// service decides, by the pairing phase, when the panel appears and
    /// when it goes.
    /// </summary>
    public static bool ShowsCode(PairingState state) =>
        state.Phase == PairingPhase.ShowCode && state.Code.Length != 0;
}
