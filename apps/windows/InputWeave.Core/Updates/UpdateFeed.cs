using System.Globalization;
using System.Security.Cryptography;
using System.Text.Json;
using Org.BouncyCastle.Math.EC.Rfc8032;

namespace InputWeave.Updates;

/// <summary>
/// The Windows update feed, <c>update-windows.json</c> of the latest release,
/// read through the website's stable address (docs/architecture.md §12.3).
/// </summary>
public sealed record UpdateFeed(
    string Version,
    int Build,
    Uri Url,
    long Size,
    string Sha256,
    byte[] Signature,
    DateTimeOffset? ReleasedAt,
    Version? MinimumOs,
    string Notes)
{
    /// <summary>Where the app reads the feed; a launch argument can point it elsewhere for testing.</summary>
    public static readonly Uri DefaultUrl = new("https://inputweave.ctsite.cc/latest/update-windows.json");

    /// <summary>Installers come only from the website's release addresses, unless the feed itself is elsewhere.</summary>
    public const string ReleasesPrefix = "https://inputweave.ctsite.cc/download/";

    /// <summary>A feed is at most this big.</summary>
    public const int MaxBytes = 256 * 1024;

    /// <summary>
    /// The feed, or null when it is not one: missing or malformed fields,
    /// or an installer URL outside <paramref name="allowedPrefix"/>.
    /// Unknown fields are ignored.
    /// </summary>
    public static UpdateFeed? Parse(string json, string allowedPrefix)
    {
        try
        {
            using var document = JsonDocument.Parse(json);
            var root = document.RootElement;
            if (root.ValueKind != JsonValueKind.Object)
            {
                return null;
            }
            string? version = Str(root, "version");
            string? url = Str(root, "url");
            string? sha256 = Str(root, "sha256")?.ToLowerInvariant();
            string? signature = Str(root, "ed25519");
            if (version is null || url is null || sha256 is null || signature is null
                || !root.TryGetProperty("build", out var build) || !build.TryGetInt32(out var buildNumber) || buildNumber <= 0
                || !root.TryGetProperty("size", out var size) || !size.TryGetInt64(out var sizeBytes) || sizeBytes <= 0
                || sha256.Length != 64 || !sha256.All(Uri.IsHexDigit)
                || !Uri.TryCreate(url, UriKind.Absolute, out var uri) || !Under(uri, allowedPrefix))
            {
                return null;
            }
            byte[] signatureBytes;
            try
            {
                signatureBytes = Convert.FromBase64String(signature);
            }
            catch (FormatException)
            {
                return null;
            }
            if (signatureBytes.Length != 64)
            {
                return null;
            }
            DateTimeOffset? released = Str(root, "released_at") is { } r
                && DateTimeOffset.TryParse(r, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var at) ? at : null;
            System.Version? minimum = Str(root, "minimum_os") is { } m && System.Version.TryParse(m, out var v) ? v : null;
            return new UpdateFeed(version, buildNumber, uri, sizeBytes, sha256, signatureBytes, released, minimum, Str(root, "notes") ?? "");
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <summary>
    /// <paramref name="uri"/>, as parsed and normalized, is under
    /// <paramref name="prefix"/>: the same scheme (https, or http for a
    /// test feed's own folder), host and port, no user info, and a path
    /// below the prefix's with nothing escaped in it (so no encoded dots or
    /// slashes reach further up).
    /// </summary>
    public static bool Under(Uri uri, string prefix)
    {
        if (!Uri.TryCreate(prefix, UriKind.Absolute, out var root) || !root.AbsolutePath.EndsWith('/'))
        {
            return false;
        }
        return uri.Scheme == root.Scheme
            && uri.Scheme is "https" or "http"
            && string.Equals(uri.Host, root.Host, StringComparison.OrdinalIgnoreCase)
            && uri.Port == root.Port
            && uri.UserInfo.Length == 0
            && !uri.AbsolutePath.Contains('%')
            && uri.AbsolutePath.Length > root.AbsolutePath.Length
            && uri.AbsolutePath.StartsWith(root.AbsolutePath, StringComparison.Ordinal);
    }

    /// <summary>The installer prefix a feed at <paramref name="feed"/> may name: the website's release addresses, or the feed's own folder for a test feed.</summary>
    public static string PrefixFor(Uri feed) =>
        feed == DefaultUrl ? ReleasesPrefix : new Uri(feed, ".").AbsoluteUri;

    private static string? Str(JsonElement root, string name) =>
        root.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
}

/// <summary>What a feed means for this app.</summary>
public enum UpdateVerdict
{
    UpToDate,
    Available,
    /// <summary>Newer, but the user chose Skip This Version for it.</summary>
    Skipped,
    /// <summary>Newer, but it does not install on this Windows (minimum_os).</summary>
    NeedsNewerWindows,
}

public static class UpdateRules
{
    /// <summary>Automatic checks happen about once a day.</summary>
    public static readonly TimeSpan CheckEvery = TimeSpan.FromDays(1);

    /// <summary>After a failed check, the next automatic one waits this long.</summary>
    public static readonly TimeSpan RetryAfterFailure = TimeSpan.FromHours(1);

    /// <summary>The build number of a version: major × 10000 + minor × 100 + patch, as Sparkle's CFBundleVersion.</summary>
    public static int BuildOf(Version version) =>
        version.Major * 10000 + version.Minor * 100 + Math.Max(0, version.Build);

    public static UpdateVerdict Judge(UpdateFeed feed, int ownBuild, string? skippedVersion, Version os)
    {
        if (feed.Build <= ownBuild)
        {
            return UpdateVerdict.UpToDate;
        }
        if (feed.MinimumOs is { } minimum && Compare(os, minimum) < 0)
        {
            return UpdateVerdict.NeedsNewerWindows;
        }
        return feed.Version == skippedVersion ? UpdateVerdict.Skipped : UpdateVerdict.Available;
    }

    /// <summary>Whether an automatic check is due.</summary>
    public static bool CheckDue(bool automatic, DateTimeOffset? lastChecked, DateTimeOffset? lastFailed, DateTimeOffset now) =>
        automatic
        && (lastChecked is not { } checkedAt || now - checkedAt >= CheckEvery || checkedAt > now)
        && (lastFailed is not { } failedAt || now - failedAt >= RetryAfterFailure || failedAt > now);

    /// <summary>Compares major.minor.build only, as minimum_os names them.</summary>
    private static int Compare(Version a, Version b) =>
        (a.Major, a.Minor, Math.Max(0, a.Build)).CompareTo((b.Major, b.Minor, Math.Max(0, b.Build)));
}

/// <summary>How a downloaded installer compares with its feed.</summary>
public enum InstallerCheck
{
    Good,
    WrongSize,
    WrongHash,
    BadSignature,
}

/// <summary>
/// Checks a downloaded MSI against its feed: its size, SHA-256 and the
/// Ed25519 signature (RFC 8032) over its bytes, made with the same key as
/// Sparkle's for macOS.
/// </summary>
public static class UpdateSignature
{
    /// <summary>InputWeave's update key (SUPublicEDKey in the macOS app).</summary>
    public const string PublicKey = "UorCjys4eeYiG7ivmGdNh4KTWhvDdHFumBtYrctpY6o=";

    public static InstallerCheck Check(byte[] installer, UpdateFeed feed, byte[] publicKey)
    {
        if (installer.LongLength != feed.Size)
        {
            return InstallerCheck.WrongSize;
        }
        var hash = Convert.ToHexString(SHA256.HashData(installer)).ToLowerInvariant();
        if (!CryptographicOperations.FixedTimeEquals(System.Text.Encoding.ASCII.GetBytes(hash), System.Text.Encoding.ASCII.GetBytes(feed.Sha256)))
        {
            return InstallerCheck.WrongHash;
        }
        return Verify(installer, feed.Signature, publicKey) ? InstallerCheck.Good : InstallerCheck.BadSignature;
    }

    public static bool Verify(byte[] message, byte[] signature, byte[] publicKey)
    {
        if (signature.Length != Ed25519.SignatureSize || publicKey.Length != Ed25519.PublicKeySize)
        {
            return false;
        }
        try
        {
            return Ed25519.Verify(signature, 0, publicKey, 0, message, 0, message.Length);
        }
        catch (ArgumentException)
        {
            return false;
        }
    }
}
