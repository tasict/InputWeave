using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using Inputweave.App.V1;

namespace InputWeave.Licensing;

/// <summary>What one piece of license work does.</summary>
public enum LicenseOpKind
{
    /// <summary>Ask Polar to activate this computer for the key the background service waits to activate.</summary>
    Activate,

    /// <summary>Give the service an activation Polar granted (RecordLicenseResult, activated).</summary>
    Report,

    /// <summary>Give the service Polar's refusal of the key or of its activation (seats full, or invalid).</summary>
    Refuse,

    /// <summary>Ask Polar to confirm the service's activation; only while the License page or About is open.</summary>
    Confirm,

    /// <summary>Give the service Polar's confirmation.</summary>
    Confirmed,

    /// <summary>Free an activation with Polar (deactivate); runs even while the service is out of reach.</summary>
    Free,

    /// <summary>Remove the license from the service (RemoveLicense).</summary>
    Remove,
}

/// <summary>Why the last attempt at a piece of work did not finish it.</summary>
public enum LicenseOpFailure
{
    None,
    /// <summary>Polar could not be reached.</summary>
    Unreachable,
    /// <summary>Polar's rate limit (429).</summary>
    RateLimited,
    /// <summary>Polar could not read the request, and not because of the key (422).</summary>
    Rejected,
    /// <summary>The background service could not be reached, or did not take it.</summary>
    Service,
    /// <summary>The background service cannot read its license file now.</summary>
    LicenseUnavailable,
}

/// <summary>
/// One piece of license work the app owes. Each is safe to run again: it
/// looks at the service's license before it acts and is dropped when there
/// is nothing left to do.
/// </summary>
public sealed record LicenseOp
{
    /// <summary>Tells this piece of work from an equal one.</summary>
    public string Id { get; init; } = Guid.NewGuid().ToString("N");

    public LicenseOpKind Kind { get; init; }

    public string Key { get; init; } = "";

    public string ActivationId { get; init; } = "";

    public string CustomerEmail { get; init; } = "";

    public ulong ValidatedAt { get; init; }

    /// <summary>Refuse: every seat is taken; otherwise the key is invalid.</summary>
    public bool SeatsFull { get; init; }

    /// <summary>Free: this computer's own seat, given up by Remove License; the License page says so while it waits.</summary>
    public bool Removed { get; init; }

    /// <summary>Confirm: when Polar first answered 404; a second 404 at least a minute later makes the license invalid.</summary>
    public DateTimeOffset? NotFoundAt { get; init; }

    /// <summary>The attempts that did not finish it.</summary>
    public int Attempts { get; init; }

    /// <summary>Not tried again before this.</summary>
    public DateTimeOffset NextAttempt { get; init; }

    public LicenseOpFailure LastFailure { get; init; }
}

/// <summary>A key the service refused an answer for, in the state its license had then: left alone until that changes or the user acts.</summary>
public sealed record StalledKey(string Key, LicenseState State)
{
    /// <summary>Left alone because Polar's answer to activate could not be read (the license could not be saved), not because the service refused.</summary>
    public bool Unsaved { get; init; }
}

/// <summary>
/// The license work the app owes, in the order it was owed, and the key it
/// leaves alone. Kept for this user, so a restart neither activates a key
/// twice nor forgets a seat it holds.
/// </summary>
public sealed record LicenseWork
{
    public IReadOnlyList<LicenseOp> Ops { get; init; } = [];

    public StalledKey? Stalled { get; init; }

    /// <summary>Polar's Retry-After: no request to Polar before this, whoever asks.</summary>
    public DateTimeOffset PolarNotBefore { get; init; }
}

/// <summary>Where <see cref="LicenseWork"/> is kept between runs.</summary>
public interface ILicenseLedger
{
    /// <summary>The kept work; null when it is there but cannot be read now (it must then be neither replaced nor acted on).</summary>
    LicenseWork? Load();

    /// <summary>Keeps <paramref name="work"/>; false when it could not be kept (it is given again later).</summary>
    bool Save(LicenseWork work);
}

/// <summary>
/// The work in a file of this user's (%LOCALAPPDATA%\InputWeave\license-work.dat),
/// replaced in one step. It holds whole license keys, so it is JSON
/// encrypted with DPAPI for this user: other users of the computer, and a
/// copy of the file elsewhere, cannot read it. A file that cannot be read
/// now (access, I/O, DPAPI) is left as it is; one that decrypts to
/// something that is not the work is moved aside (.bad) and the work
/// starts empty. Fields it does not know are skipped.
/// </summary>
public sealed class FileLicenseLedger(string path, Action<string> log) : ILicenseLedger
{
    /// <summary>At most this many activations wait to be freed; the oldest are dropped, with a log line.</summary>
    public const int MaxToFree = 16;

    private static readonly byte[] Entropy = Encoding.UTF8.GetBytes("InputWeave license work");

    /// <summary>DPAPI for this user, with this file's entropy; public for the tests.</summary>
    public static byte[] Protect(byte[] data)
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new PlatformNotSupportedException("DPAPI is Windows only");
        }
        return ProtectedData.Protect(data, Entropy, DataProtectionScope.CurrentUser);
    }

    private static byte[] Unprotect(byte[] data)
    {
        if (!OperatingSystem.IsWindows())
        {
            throw new PlatformNotSupportedException("DPAPI is Windows only");
        }
        return ProtectedData.Unprotect(data, Entropy, DataProtectionScope.CurrentUser);
    }

    public LicenseWork? Load()
    {
        byte[] json;
        try
        {
            if (!File.Exists(path))
            {
                return new LicenseWork();
            }
            json = Unprotect(File.ReadAllBytes(path));
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or NotSupportedException or CryptographicException)
        {
            log($"license: cannot read {path} now: {e.Message}");
            return null;
        }
        try
        {
            return JsonSerializer.Deserialize(json, LicenseWorkJson.Default.LicenseWork) ?? new LicenseWork();
        }
        catch (Exception e) when (e is JsonException or NotSupportedException)
        {
            log($"license: {path} is not license work ({e.Message}); moved aside");
            try
            {
                File.Move(path, path + ".bad", overwrite: true);
            }
            catch (Exception move) when (move is IOException or UnauthorizedAccessException)
            {
                log($"license: cannot move {path} aside: {move.Message}");
                return null;
            }
            return new LicenseWork();
        }
    }

    public bool Save(LicenseWork work)
    {
        try
        {
            int frees = work.Ops.Count(o => o.Kind == LicenseOpKind.Free);
            if (frees > MaxToFree)
            {
                log($"license: {frees - MaxToFree} activations to free dropped, the oldest");
                var drop = work.Ops.Where(o => o.Kind == LicenseOpKind.Free).Take(frees - MaxToFree).ToHashSet();
                work = work with { Ops = work.Ops.Where(o => !drop.Contains(o)).ToList() };
            }
            if (work.Ops.Count == 0 && work.Stalled is null && work.PolarNotBefore == default)
            {
                File.Delete(path);
                return true;
            }
            Directory.CreateDirectory(Path.GetDirectoryName(path)!);
            var temporary = path + ".tmp";
            var json = JsonSerializer.SerializeToUtf8Bytes(work, LicenseWorkJson.Default.LicenseWork);
            File.WriteAllBytes(temporary, Protect(json));
            File.Move(temporary, path, overwrite: true);
            return true;
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException or NotSupportedException or InvalidOperationException or CryptographicException)
        {
            log($"license: cannot save {path}: {e.Message}");
            return false;
        }
    }
}

/// <summary>
/// The ledger's JSON, generated at build time: the published app is trimmed,
/// which turns off System.Text.Json's reflection-based serialization.
/// </summary>
[JsonSourceGenerationOptions(WriteIndented = true, DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull)]
[JsonSerializable(typeof(LicenseWork))]
internal sealed partial class LicenseWorkJson : JsonSerializerContext
{
}

/// <summary>The work in memory only, for the tests.</summary>
public sealed class MemoryLicenseLedger : ILicenseLedger
{
    public LicenseWork Work { get; set; } = new();

    /// <summary>Saving such work fails, as with a full disk.</summary>
    public Func<LicenseWork, bool>? FailWhen { get; set; }

    /// <summary>The work is there but cannot be read, as with a DPAPI failure.</summary>
    public bool Unreadable { get; set; }

    public LicenseWork? Load() => Unreadable ? null : Work;

    public bool Save(LicenseWork work)
    {
        if (FailWhen?.Invoke(work) == true)
        {
            return false;
        }
        Work = work;
        return true;
    }
}
