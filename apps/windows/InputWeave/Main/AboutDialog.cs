using System.Diagnostics;
using System.Globalization;
using InputWeave.Licensing;
using InputWeave.Panel;
using InputWeave.Support;
using InputWeave.Updates;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;

namespace InputWeave.Main;

/// <summary>
/// About InputWeave: the icon, the version and build,
/// "Trial" with Buy a License or the woven label with who it is licensed
/// to, the copyright and the GPL line, and Acknowledgments, which opens the
/// notices the installer puts beside the app.
/// </summary>
internal sealed partial class AboutDialog : ContentDialog
{
    private readonly StackPanel line = new() { Orientation = Orientation.Horizontal, Spacing = 8 };
    private (bool Shown, bool Licensed, string Email)? showing;

    public AboutDialog(LicenseView license)
    {
        Title = Strings.Get(TextKeys.AboutTitle);
        PrimaryButtonText = Strings.Get(TextKeys.AboutAcknowledgments);
        CloseButtonText = Strings.Get(TextKeys.Close);
        DefaultButton = ContentDialogButton.Close;
        PrimaryButtonClick += (_, _) => OpenNotices();

        var content = new StackPanel { Spacing = 12, MinWidth = 360 };
        var head = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 14 };
        head.Children.Add(new Image
        {
            Width = 48,
            Height = 48,
            Source = new BitmapImage(new Uri(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon256.png"))),
        });
        var name = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        name.Children.Add(Ui.Strong("InputWeave"));
        var version = typeof(AboutDialog).Assembly.GetName().Version ?? new Version(0, 0, 0);
        name.Children.Add(Ui.Secondary(Strings.Format(TextKeys.AboutVersion, App.Version,
            UpdateRules.BuildOf(version).ToString(CultureInfo.CurrentCulture))));
        head.Children.Add(name);
        content.Children.Add(head);

        Update(license);
        content.Children.Add(line);
        var legal = Ui.Secondary($"{Strings.Get(TextKeys.AboutCopyright)}\n{Strings.Get(TextKeys.AboutGpl)}");
        content.Children.Add(legal);
        Content = content;
    }

    /// <summary>The license line, again whenever the license changes while About is open.</summary>
    public void Update(LicenseView license)
    {
        var now = (license.Shown, license.IsLicensed, license.Email);
        if (showing == now)
        {
            return;
        }
        showing = now;
        line.Children.Clear();
        line.Visibility = license.Shown ? Visibility.Visible : Visibility.Collapsed;
        if (!license.Shown)
        {
            return;
        }
        if (license.IsLicensed)
        {
            line.Children.Add(WovenLabel.Create(24));
            if (license.Email.Length > 0)
            {
                var to = Ui.Secondary(Strings.Format(TextKeys.LicensedTo, license.Email));
                to.VerticalAlignment = VerticalAlignment.Center;
                line.Children.Add(to);
            }
        }
        else
        {
            line.Children.Add(WovenLabel.TrialPill(12));
            line.Children.Add(new HyperlinkButton
            {
                Content = Strings.Get(TextKeys.LicenseBuy),
                NavigateUri = new Uri(PolarClient.CheckoutUrl),
                Padding = new Thickness(4, 2, 4, 2),
            });
        }
    }

    private static void OpenNotices()
    {
        foreach (var candidate in new[]
        {
            Path.Combine(AppContext.BaseDirectory, "THIRD_PARTY_NOTICES.md"),
            Path.Combine(AppContext.BaseDirectory, "..", "THIRD_PARTY_NOTICES.md"),
        })
        {
            if (File.Exists(candidate))
            {
                try
                {
                    Process.Start(new ProcessStartInfo("notepad.exe") { Arguments = $"\"{Path.GetFullPath(candidate)}\"", UseShellExecute = true })?.Dispose();
                }
                catch (Exception e) when (e is System.ComponentModel.Win32Exception or InvalidOperationException)
                {
                    AppLog.Warn($"cannot open the notices: {e.Message}");
                }
                return;
            }
        }
        LicensePage.Open("https://inputweave.ctsite.cc/third-party-notices.txt");
    }
}
