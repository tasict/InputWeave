using System.Net.Http.Headers;
using System.Text;
using System.Text.Json;
using System.Text.RegularExpressions;

namespace InputWeave.Licensing;

/// <summary>What Polar's API answered to one request, as the transport saw it.</summary>
/// <param name="Status">The HTTP status.</param>
/// <param name="Body">The response body, empty when there is none.</param>
/// <param name="Date">The response's Date header, when it has one.</param>
/// <param name="RetryAfter">The Retry-After header, when it has one.</param>
public sealed record PolarReply(int Status, string Body, DateTimeOffset? Date = null, TimeSpan? RetryAfter = null);

/// <summary>
/// Sends a request to one of Polar's license-key endpoints. Throws
/// <see cref="HttpRequestException"/> (or a timeout) when Polar cannot be
/// reached; any answer, whatever its status, is a <see cref="PolarReply"/>.
/// </summary>
public interface IPolarTransport
{
    /// <param name="endpoint"><c>activate</c>, <c>validate</c> or <c>deactivate</c>.</param>
    Task<PolarReply> SendAsync(string endpoint, string json, CancellationToken cancellation);
}

/// <summary>The real transport: HTTPS to Polar's public customer-portal endpoints, which need no token.</summary>
public sealed class HttpPolarTransport : IPolarTransport, IDisposable
{
    /// <summary>Polar's API; a launch argument can point the app elsewhere for testing.</summary>
    public static readonly Uri DefaultServer = new("https://api.polar.sh/");

    /// <summary>How long validate and deactivate wait for Polar.</summary>
    public static readonly TimeSpan Timeout = TimeSpan.FromSeconds(20);

    /// <summary>
    /// How long activate waits: a 200 that is given up on may still have
    /// taken a seat, so it is waited for longer.
    /// </summary>
    public static readonly TimeSpan ActivateTimeout = TimeSpan.FromSeconds(60);

    private readonly HttpClient http;
    private readonly Uri server;

    /// <param name="userAgent">
    /// <c>InputWeave/&lt;version&gt;</c>: Cloudflare, in front of Polar,
    /// answers some default user agents with a 403 of its own.
    /// </param>
    public HttpPolarTransport(Uri server, string userAgent)
    {
        this.server = server;
        http = new HttpClient { Timeout = System.Threading.Timeout.InfiniteTimeSpan };
        http.DefaultRequestHeaders.UserAgent.ParseAdd(userAgent);
        http.DefaultRequestHeaders.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
    }

    public async Task<PolarReply> SendAsync(string endpoint, string json, CancellationToken cancellation)
    {
        var uri = new Uri(server, $"v1/customer-portal/license-keys/{endpoint}");
        using var content = new StringContent(json, Encoding.UTF8, "application/json");
        using var timeout = CancellationTokenSource.CreateLinkedTokenSource(cancellation);
        timeout.CancelAfter(endpoint == "activate" ? ActivateTimeout : Timeout);
        using var response = await http.PostAsync(uri, content, timeout.Token).ConfigureAwait(false);
        var body = await response.Content.ReadAsStringAsync(timeout.Token).ConfigureAwait(false);
        TimeSpan? retry = response.Headers.RetryAfter switch
        {
            { Delta: { } delta } => delta,
            { Date: { } date } => date - DateTimeOffset.UtcNow,
            _ => null,
        };
        return new PolarReply((int)response.StatusCode, body, response.Headers.Date, retry);
    }

    public void Dispose() => http.Dispose();
}

/// <summary>What one request to Polar came to.</summary>
public abstract record PolarAnswer
{
    private PolarAnswer()
    {
    }

    /// <summary>activate 200: this computer has a seat.</summary>
    public sealed record Activated(string ActivationId, string CustomerEmail, ulong ValidatedAt) : PolarAnswer;

    /// <summary>validate 200 with an activation id: this computer's activation stands.</summary>
    public sealed record Validated(string CustomerEmail, ulong ValidatedAt) : PolarAnswer;

    /// <summary>validate 200 without an activation id: the key itself is live (after a 403 from activate, every seat is taken).</summary>
    public sealed record KeyLive : PolarAnswer;

    /// <summary>A 403 whose body says NotPermitted: every seat is taken, or the key is no longer active.</summary>
    public sealed record NotPermitted : PolarAnswer;

    /// <summary>404: the key is unknown, revoked or refunded, or the activation is gone.</summary>
    public sealed record NotFound : PolarAnswer;

    /// <summary>
    /// 422: Polar could not read the request. <paramref name="AboutKey"/>:
    /// what it could not read was the key (a key of the wrong shape, so
    /// invalid); <paramref name="AboutActivation"/>: the activation id (so
    /// no such activation). Otherwise the fault is not the license's.
    /// </summary>
    public sealed record Malformed(bool AboutKey, bool AboutActivation = false) : PolarAnswer;

    /// <summary>429: too many requests; try again after <paramref name="RetryAfter"/>.</summary>
    public sealed record RateLimited(TimeSpan RetryAfter) : PolarAnswer;

    /// <summary>deactivate 204, or 404 because the activation is already gone.</summary>
    public sealed record Done : PolarAnswer;

    /// <summary>
    /// activate 200 whose body has no activation id: Polar may have taken a
    /// seat, so it is not asked again by itself.
    /// </summary>
    public sealed record Unreadable(string Reason) : PolarAnswer;

    /// <summary>
    /// No answer from Polar itself: no network, a timeout, a 5xx, a 403
    /// from Cloudflare, or a body the app cannot read. Try again later;
    /// never a reason to change the license.
    /// </summary>
    public sealed record Unreachable(string Reason) : PolarAnswer;
}

/// <summary>
/// Polar's three license-key endpoints, as docs/architecture.md §11 says
/// the apps use them: the key, the organization id and this computer's
/// name as the label, never <c>conditions</c>. Pure apart from the
/// transport, so the tests feed it the bodies in testdata/polar.
/// </summary>
public sealed class PolarClient(IPolarTransport transport, string organizationId, Func<DateTimeOffset> clock)
{
    /// <summary>InputWeave's organization on Polar.</summary>
    public const string OrganizationId = "95b9d4a7-5dad-4df5-b309-2cd3383420ae";

    /// <summary>Where to buy a license.</summary>
    public const string CheckoutUrl = "https://buy.polar.sh/polar_cl_QlXeUC208E1POhQcAZe6AgpFHGbpsKGBXmhmu0x5LVd";

    /// <summary>Where a buyer frees computers on the license.</summary>
    public const string PortalUrl = "https://polar.sh/ctsitecc/portal";

    /// <summary>Validated-at times before this are refused by the agent (2020-01-01).</summary>
    private static readonly DateTimeOffset Earliest = new(2020, 1, 1, 0, 0, 0, TimeSpan.Zero);

    /// <summary>The longest label sent; longer computer names are cut.</summary>
    public const int MaxLabel = 64;

    public Task<PolarAnswer> ActivateAsync(string key, string label, CancellationToken cancellation = default) =>
        SendAsync("activate", Json(w =>
        {
            w.WriteString("key", key);
            w.WriteString("organization_id", organizationId);
            w.WriteString("label", Label(label));
        }), cancellation);

    /// <summary>
    /// The label for this computer's activation: its name on one line,
    /// spaces collapsed, at most <see cref="MaxLabel"/> characters (never
    /// half a character), and never empty.
    /// </summary>
    public static string Label(string name)
    {
        var line = Regex.Replace(name, @"[\s\p{Cc}\p{Cf}]+", " ").Trim();
        var info = new System.Globalization.StringInfo(line);
        if (info.LengthInTextElements > MaxLabel)
        {
            line = info.SubstringByTextElements(0, MaxLabel).TrimEnd();
        }
        return line.Length > 0 ? line : "InputWeave";
    }

    /// <param name="activationId">Null to ask about the key alone (after a 403 from activate).</param>
    public Task<PolarAnswer> ValidateAsync(string key, string? activationId, CancellationToken cancellation = default) =>
        SendAsync("validate", Json(w =>
        {
            w.WriteString("key", key);
            w.WriteString("organization_id", organizationId);
            if (!string.IsNullOrEmpty(activationId))
            {
                w.WriteString("activation_id", activationId);
            }
        }), cancellation, keyOnly: string.IsNullOrEmpty(activationId));

    public Task<PolarAnswer> DeactivateAsync(string key, string activationId, CancellationToken cancellation = default) =>
        SendAsync("deactivate", Json(w =>
        {
            w.WriteString("key", key);
            w.WriteString("organization_id", organizationId);
            w.WriteString("activation_id", activationId);
        }), cancellation);

    private async Task<PolarAnswer> SendAsync(string endpoint, string json, CancellationToken cancellation, bool keyOnly = false)
    {
        PolarReply reply;
        try
        {
            reply = await transport.SendAsync(endpoint, json, cancellation).ConfigureAwait(false);
        }
        catch (Exception e) when (e is HttpRequestException or OperationCanceledException or TimeoutException or IOException)
        {
            if (cancellation.IsCancellationRequested)
            {
                throw;
            }
            return new PolarAnswer.Unreachable(e.Message);
        }
        return Read(endpoint, reply, keyOnly);
    }

    /// <summary>What a reply means; public for the tests.</summary>
    public PolarAnswer Read(string endpoint, PolarReply reply, bool keyOnly = false)
    {
        switch (reply.Status)
        {
            case 200 or 201 when endpoint == "activate":
                return Parse(reply.Body) is { } a && Str(a, "id") is { Length: > 0 } id
                    ? new PolarAnswer.Activated(id, Email(Child(a, "license_key")), When(reply.Date))
                    : new PolarAnswer.Unreadable("activate answered without an activation id");
            case 200 when endpoint == "validate":
                if (Parse(reply.Body) is not { } v)
                {
                    return new PolarAnswer.Unreachable("validate answered without a body");
                }
                return keyOnly ? new PolarAnswer.KeyLive() : new PolarAnswer.Validated(Email(v), When(reply.Date));
            case 200 or 204 when endpoint == "deactivate":
                return new PolarAnswer.Done();
            case 404 when endpoint == "deactivate":
                // Already gone: freed all the same.
                return new PolarAnswer.Done();
            case 403:
                // Only Polar's own refusal counts; Cloudflare's 403 has no Polar error.
                return Parse(reply.Body) is { } refusal && Str(refusal, "error") == "NotPermitted"
                    ? new PolarAnswer.NotPermitted()
                    : new PolarAnswer.Unreachable("403 without Polar's error");
            case 404:
                return new PolarAnswer.NotFound();
            case 422:
                return new PolarAnswer.Malformed(Names(reply.Body, "key"), Names(reply.Body, "activation_id"));
            case 429:
                // Without a usable Retry-After a minute; never more than an hour.
                var retry = reply.RetryAfter is { } r && r > TimeSpan.Zero ? r : TimeSpan.FromSeconds(60);
                return new PolarAnswer.RateLimited(retry > TimeSpan.FromHours(1) ? TimeSpan.FromHours(1) : retry);
            default:
                return new PolarAnswer.Unreachable($"HTTP {reply.Status}");
        }
    }

    /// <summary>
    /// When Polar answered: its Date header, or this computer's clock when
    /// the header is missing, before 2020, or more than a day ahead (the
    /// agent refuses those).
    /// </summary>
    private ulong When(DateTimeOffset? date)
    {
        var now = clock();
        var at = date is { } d && d >= Earliest && d <= now.AddDays(1) ? d : now;
        return (ulong)Math.Max(0, at.ToUnixTimeSeconds());
    }

    private static string Json(Action<Utf8JsonWriter> write)
    {
        using var buffer = new MemoryStream();
        using (var writer = new Utf8JsonWriter(buffer))
        {
            writer.WriteStartObject();
            write(writer);
            writer.WriteEndObject();
        }
        return Encoding.UTF8.GetString(buffer.ToArray());
    }

    private static JsonElement? Parse(string body)
    {
        if (string.IsNullOrWhiteSpace(body))
        {
            return null;
        }
        try
        {
            using var document = JsonDocument.Parse(body);
            return document.RootElement.ValueKind == JsonValueKind.Object ? document.RootElement.Clone() : null;
        }
        catch (JsonException)
        {
            return null;
        }
    }

    /// <summary>A 422's detail names <paramref name="field"/>, as in <c>"loc": ["body", "key"]</c>.</summary>
    private static bool Names(string body, string field)
    {
        if (Parse(body) is not { } reply || !reply.TryGetProperty("detail", out var detail) || detail.ValueKind != JsonValueKind.Array)
        {
            return false;
        }
        foreach (var item in detail.EnumerateArray())
        {
            if (item.ValueKind == JsonValueKind.Object && item.TryGetProperty("loc", out var loc) && loc.ValueKind == JsonValueKind.Array
                && loc.EnumerateArray().Any(part => part.ValueKind == JsonValueKind.String && part.GetString() == field))
            {
                return true;
            }
        }
        return false;
    }

    private static JsonElement? Child(JsonElement? parent, string name) =>
        parent is { } p && p.TryGetProperty(name, out var child) && child.ValueKind == JsonValueKind.Object ? child : null;

    private static string? Str(JsonElement? element, string name) =>
        element is { } e && e.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;

    /// <summary>The buyer's address, from an object with a <c>customer</c> in it.</summary>
    private static string Email(JsonElement? withCustomer) => Str(Child(withCustomer, "customer"), "email") ?? "";
}

/// <summary>What Polar answered, as a log line (never the key).</summary>
public static class PolarLog
{
    public static string Describe(PolarAnswer answer) => answer switch
    {
        PolarAnswer.Activated => "activated",
        PolarAnswer.Validated => "confirmed",
        PolarAnswer.KeyLive => "key live",
        PolarAnswer.NotPermitted => "not permitted",
        PolarAnswer.NotFound => "not found",
        PolarAnswer.Malformed m => m.AboutKey ? "the key is malformed" : m.AboutActivation ? "no such activation" : "malformed request",
        PolarAnswer.RateLimited r => $"rate limited for {r.RetryAfter.TotalSeconds:0} s",
        PolarAnswer.Done => "done",
        PolarAnswer.Unreachable u => $"unreachable ({u.Reason})",
        PolarAnswer.Unreadable u => $"unreadable ({u.Reason})",
        _ => answer.GetType().Name,
    };
}
