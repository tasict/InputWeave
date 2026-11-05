using System.Security.Cryptography;
using System.Text;
using InputWeave.Updates;
using Xunit;

namespace InputWeave.Tests;

public class UpdateFeedTests
{
    // A throwaway key made with OpenSSL for these tests only, and its
    // signature over Message; never InputWeave's update key.
    private const string ThrowawayKey = "hzXMkZO2vDw+vZHSulUUpfAxoBD57648XO1HTyd2hGE=";
    private const string MessageSignature = "FxXCSqZcgPu156DvmTnzwQiOYJ0p9KLe1E4y7F2eEzgk7CRJH23NIYNwl2yA+dCGadHBBkQOeVwKUOkDg0RrAQ==";
    private static readonly byte[] Message = Encoding.ASCII.GetBytes("InputWeave update test vector\n");

    private static string Feed(string url = UpdateFeed.ReleasesPrefix + "v1.0.1/InputWeave-1.0.1-x64.msi", string extra = "", int build = 10001) => $$"""
        {
          "version": "1.0.1",
          "build": {{build}},
          "url": "{{url}}",
          "size": {{Message.Length}},
          "sha256": "{{Convert.ToHexString(SHA256.HashData(Message)).ToLowerInvariant()}}",
          "ed25519": "{{MessageSignature}}",
          "released_at": "2026-11-04T08:00:00Z",
          "minimum_os": "10.0.19045",
          "notes": "Fixes.\n\n- One\n- Two"{{extra}}
        }
        """;

    [Fact]
    public void ReadsTheFeedAndIgnoresUnknownFields()
    {
        var feed = UpdateFeed.Parse(Feed(extra: ", \"later\": [1, 2]"), UpdateFeed.ReleasesPrefix);
        Assert.NotNull(feed);
        Assert.Equal("1.0.1", feed.Version);
        Assert.Equal(10001, feed.Build);
        Assert.Equal(new Version(10, 0, 19045), feed.MinimumOs);
        Assert.Equal(new DateTimeOffset(2026, 11, 4, 8, 0, 0, TimeSpan.Zero), feed.ReleasedAt);
        Assert.Contains("- Two", feed.Notes);
    }

    [Theory]
    [InlineData("https://example.com/InputWeave-1.0.1-x64.msi")]
    [InlineData("https://inputweave.ctsite.cc/downloads/v1.0.1/InputWeave-1.0.1-x64.msi")]
    [InlineData("https://inputweave.ctsite.cc/latest/InputWeave-1.0.1-x64.msi")]
    [InlineData("https://inputweave.ctsite.cc/download/../../someone/x.msi")]
    [InlineData("https://inputweave.ctsite.cc/download/%2e%2e/x.msi")]
    [InlineData("https://inputweave.ctsite.cc.example.com/download/v1.0.1/x.msi")]
    [InlineData("https://inputweave.ctsite.cc@example.com/download/v1.0.1/x.msi")]
    [InlineData("https://user@inputweave.ctsite.cc/download/v1.0.1/x.msi")]
    [InlineData("http://inputweave.ctsite.cc/download/v1.0.1/x.msi")]
    [InlineData("https://inputweave.ctsite.cc:8443/download/v1.0.1/x.msi")]
    [InlineData("https://github.com/tasict/InputWeave/releases/download/v1.0.1/InputWeave-1.0.1-x64.msi")]
    public void RefusesAnInstallerFromElsewhere(string url) => Assert.Null(UpdateFeed.Parse(Feed(url), UpdateFeed.ReleasesPrefix));

    [Theory]
    [InlineData("{}")]
    [InlineData("[]")]
    [InlineData("not json")]
    public void RefusesWhatIsNotAFeed(string json) => Assert.Null(UpdateFeed.Parse(json, UpdateFeed.ReleasesPrefix));

    [Fact]
    public void RefusesABadSignatureField() =>
        Assert.Null(UpdateFeed.Parse(Feed().Replace(MessageSignature, "c2hvcnQ="), UpdateFeed.ReleasesPrefix));

    [Fact]
    public void ATestFeedMayNameInstallersBesideIt()
    {
        var at = new Uri("http://127.0.0.1:8765/feed/update-windows.json");
        Assert.Equal("http://127.0.0.1:8765/feed/", UpdateFeed.PrefixFor(at));
        Assert.Equal(UpdateFeed.ReleasesPrefix, UpdateFeed.PrefixFor(UpdateFeed.DefaultUrl));
        Assert.NotNull(UpdateFeed.Parse(Feed("http://127.0.0.1:8765/feed/InputWeave-1.0.1-x64.msi"), UpdateFeed.PrefixFor(at)));
    }

    [Fact]
    public void ComparesBuildsNotVersions()
    {
        var feed = UpdateFeed.Parse(Feed(), UpdateFeed.ReleasesPrefix)!;
        var windows = new Version(10, 0, 26100);
        Assert.Equal(10000, UpdateRules.BuildOf(new Version(1, 0, 0, 0)));
        Assert.Equal(10203, UpdateRules.BuildOf(new Version(1, 2, 3)));
        Assert.Equal(UpdateVerdict.Available, UpdateRules.Judge(feed, 10000, null, windows));
        Assert.Equal(UpdateVerdict.UpToDate, UpdateRules.Judge(feed, 10001, null, windows));
        Assert.Equal(UpdateVerdict.UpToDate, UpdateRules.Judge(feed, 20000, null, windows));
        Assert.Equal(UpdateVerdict.Skipped, UpdateRules.Judge(feed, 10000, "1.0.1", windows));
        Assert.Equal(UpdateVerdict.Available, UpdateRules.Judge(feed, 10000, "1.0.0", windows));
        Assert.Equal(UpdateVerdict.NeedsNewerWindows, UpdateRules.Judge(feed, 10000, null, new Version(10, 0, 19044, 1)));
    }

    [Fact]
    public void ChecksAboutOnceADayWhenOn()
    {
        var now = new DateTimeOffset(2026, 11, 5, 12, 0, 0, TimeSpan.Zero);
        Assert.True(UpdateRules.CheckDue(true, null, null, now));
        Assert.False(UpdateRules.CheckDue(false, null, null, now));
        Assert.False(UpdateRules.CheckDue(true, now.AddHours(-23), null, now));
        Assert.True(UpdateRules.CheckDue(true, now.AddHours(-25), null, now));
        Assert.False(UpdateRules.CheckDue(true, now.AddDays(-2), now.AddMinutes(-30), now));
        Assert.True(UpdateRules.CheckDue(true, now.AddDays(-2), now.AddHours(-2), now));
        // A clock set back: check again rather than never.
        Assert.True(UpdateRules.CheckDue(true, now.AddDays(3), null, now));
    }

    [Fact]
    public void VerifiesTheSignatureOverTheInstaller()
    {
        var feed = UpdateFeed.Parse(Feed(), UpdateFeed.ReleasesPrefix)!;
        var key = Convert.FromBase64String(ThrowawayKey);
        Assert.Equal(InstallerCheck.Good, UpdateSignature.Check(Message, feed, key));

        var tampered = (byte[])Message.Clone();
        tampered[3] ^= 1;
        Assert.Equal(InstallerCheck.WrongHash, UpdateSignature.Check(tampered, feed, key));
        Assert.False(UpdateSignature.Verify(tampered, feed.Signature, key));

        Assert.Equal(InstallerCheck.BadSignature, UpdateSignature.Check(Message, feed, Convert.FromBase64String(UpdateSignature.PublicKey)));
        Assert.Equal(InstallerCheck.WrongSize, UpdateSignature.Check([.. Message, 0], feed, key));
    }

    [Fact]
    public void TheUpdateKeyIsAnEd25519PublicKey() => Assert.Equal(32, Convert.FromBase64String(UpdateSignature.PublicKey).Length);
}
