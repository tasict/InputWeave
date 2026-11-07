using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace InputWeave.Main;

/// <summary>
/// Profiles: the profile in use (Automatic, or one the user chose), the
/// network this computer is on with Add to Profile, every profile, each
/// opening its own page (titled "Profiles › Office" in the window's header),
/// and recognizing networks by their Wi-Fi name too. A profile's page has
/// its name, sharing or pausing, the computers it connects, the clipboard
/// and files, its networks, and Remove Profile. Everything is the service's:
/// the page shows its status and sends the user's changes.
/// </summary>
internal sealed partial class ProfilesPage : UserControl, IMainPage
{
    private readonly Grid root = new();
    private readonly ScrollViewer listView;
    private readonly ScrollViewer profileView = new()
    {
        HorizontalScrollMode = ScrollMode.Disabled,
        VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        Visibility = Visibility.Collapsed,
    };

    // The list of profiles.
    private readonly ComboBox inUse = new() { MinWidth = 240 };
    private readonly FontIcon networkIcon = new();
    private readonly TextBlock networkTitle = new() { TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock networkDetail = Ui.Secondary("");
    private readonly DropDownButton addTo = new() { Content = Strings.Get(TextKeys.ProfilesAddTo) };
    private readonly MenuFlyout addToMenu = new() { Placement = Microsoft.UI.Xaml.Controls.Primitives.FlyoutPlacementMode.BottomEdgeAlignedRight };
    private readonly StackPanel rows = new() { Spacing = 4 };
    private readonly Button addProfile;
    private readonly ToggleSwitch wifiNames;
    private string shownChoices = "";
    private string shownRows = "";
    /// <summary>The ids of the profiles listed in "Profile in use", after Automatic and the separator.</summary>
    private List<string> choiceIds = [];

    private ProfileView? profile;
    private AgentSnapshot? model;
    private bool updating;

    public ProfilesPage()
    {
        // The list.
        AutomationProperties.SetName(inUse, Strings.Get(TextKeys.ProfilesInUse));
        inUse.SelectionChanged += (_, _) => ChooseProfile();
        addTo.Flyout = addToMenu;
        addToMenu.Opening += (_, _) => FillAddTo();
        var networkWords = new StackPanel { Spacing = 2 };
        networkWords.Children.Add(networkTitle);
        networkWords.Children.Add(networkDetail);
        addProfile = Ui.Button(Strings.Get(TextKeys.ProfilesAdd), AddProfile);
        wifiNames = Ui.Switch(Strings.Get(TextKeys.ProfilesWifiNames), false, on =>
        {
            if (on)
            {
                App.Instance.WifiNames.TurnOn();
            }
            else
            {
                App.Instance.WifiNames.TurnOff();
            }
        });

        var list = new StackPanel();
        var top = Ui.Card(
            Ui.Row(Strings.Get(TextKeys.ProfilesInUse), Strings.Get(TextKeys.ProfilesInUseHint), inUse),
            Ui.Row(networkWords, addTo, networkIcon));
        top.Margin = new Thickness(0, 12, 0, 0);
        list.Children.Add(top);
        list.Children.Add(Ui.Header(Strings.Get(TextKeys.ProfilesAll), addProfile));
        list.Children.Add(rows);
        var wifi = Ui.Card(Ui.Row(Strings.Get(TextKeys.ProfilesWifiNames), Strings.Get(TextKeys.ProfilesWifiNamesHint), wifiNames));
        wifi.Margin = new Thickness(0, 16, 0, 0);
        list.Children.Add(wifi);
        listView = Ui.Page(list);

        root.Children.Add(listView);
        root.Children.Add(profileView);
        Content = root;
        // A note under a profile's name goes as the page does.
        Unloaded += (_, _) => profile?.ClearNameNote();
        Header = Strings.Get(TextKeys.NavProfiles);
    }

    /// <summary>The window's header for this page: "Profiles", or "Profiles › Office" on a profile's page.</summary>
    public object Header { get; private set; }

    /// <summary>Raised when <see cref="Header"/> changes.</summary>
    public event Action? HeaderChanged;

    public void Invalidate()
    {
        model = null;
        shownChoices = "";
        shownRows = "";
        profile?.Invalidate();
    }

    public void Update(AgentSnapshot snapshot, ServiceState service, DateTimeOffset now)
    {
        if (ReferenceEquals(snapshot, model))
        {
            return;
        }
        model = snapshot;
        if (profile is not null)
        {
            // A profile removed meanwhile, here or by its own Remove Profile.
            if (Find(snapshot, profile.Id) is not { } shown)
            {
                ShowList();
            }
            else
            {
                profile.Update(snapshot, shown);
                SetHeader(Strings.Text(Profiles.Name(shown)));
                return;
            }
        }
        UpdateList(snapshot);
    }

    private static Profile? Find(AgentSnapshot snapshot, string id) => snapshot.Status.Profiles.FirstOrDefault(p => p.Id == id);

    /// <summary>Back to the list of profiles, from a profile's page.</summary>
    internal void ShowList()
    {
        if (profile is null)
        {
            return;
        }
        profile = null;
        profileView.Content = null;
        profileView.Visibility = Visibility.Collapsed;
        listView.Visibility = Visibility.Visible;
        SetHeader(null);
        if (model is { } snapshot)
        {
            UpdateList(snapshot);
        }
    }

    /// <summary>A profile's page; its name field takes the focus for a profile just added.</summary>
    private void ShowProfile(string id, bool focusName = false)
    {
        if (model is not { } snapshot || Find(snapshot, id) is not { } shown)
        {
            return;
        }
        profile = new ProfileView(id);
        profileView.Content = profile.View;
        profileView.ChangeView(null, 0, null, disableAnimation: true);
        listView.Visibility = Visibility.Collapsed;
        profileView.Visibility = Visibility.Visible;
        profile.Update(snapshot, shown);
        SetHeader(Strings.Text(Profiles.Name(shown)));
        if (focusName)
        {
            var view = profile;
            DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, view.FocusName);
        }
    }

    /// <summary>"Profiles", or the breadcrumb "Profiles › Office", whose first part goes back to the list.</summary>
    private void SetHeader(string? profileName)
    {
        if (profileName is null)
        {
            if (Header is string)
            {
                return;
            }
            Header = Strings.Get(TextKeys.NavProfiles);
        }
        else
        {
            if (Header is BreadcrumbBar { ItemsSource: string[] parts } && parts[1] == profileName)
            {
                return;
            }
            var bar = new BreadcrumbBar { ItemsSource = new[] { Strings.Get(TextKeys.NavProfiles), profileName } };
            // As large as the other pages' titles.
            bar.Resources["BreadcrumbBarItemThemeFontSize"] = 28.0;
            bar.Resources["BreadcrumbBarItemFontWeight"] = Microsoft.UI.Text.FontWeights.SemiBold;
            bar.Resources["BreadcrumbBarChevronFontSize"] = 16.0;
            bar.ItemClicked += (_, e) =>
            {
                if (e.Index == 0)
                {
                    ShowList();
                }
            };
            Header = bar;
        }
        HeaderChanged?.Invoke();
    }

    private void UpdateList(AgentSnapshot snapshot)
    {
        bool connected = snapshot.Link == AgentLink.Connected;
        var status = snapshot.Status;

        // Profile in use: Automatic, a separator, each profile.
        var automatic = Strings.Text(Profiles.AutomaticLabel(snapshot));
        var names = status.Profiles.Select(p => Strings.Text(Profiles.Name(p))).ToList();
        var choices = $"{automatic}|{string.Join("|", status.Profiles.Select((p, i) => $"{p.Id}={names[i]}"))}";
        updating = true;
        if (choices != shownChoices)
        {
            shownChoices = choices;
            inUse.Items.Clear();
            inUse.Items.Add(new ComboBoxItem { Content = automatic });
            inUse.Items.Add(Separator());
            foreach (var name in names)
            {
                inUse.Items.Add(new ComboBoxItem { Content = name });
            }
            choiceIds = status.Profiles.Select(p => p.Id).ToList();
        }
        var chosen = Profiles.Chosen(snapshot);
        int index = chosen.Length == 0 ? 0 : choiceIds.IndexOf(chosen) + 2;
        inUse.SelectedIndex = index >= 2 || chosen.Length == 0 ? index : 0;
        inUse.IsEnabled = connected;
        updating = false;

        // The network this computer is on.
        networkTitle.Text = Strings.Text(Profiles.CurrentNetwork(snapshot));
        if (status.Network is { } network)
        {
            networkIcon.Glyph = Profiles.NetworkGlyph(network);
            networkIcon.Visibility = Visibility.Visible;
            var detail = Profiles.NetworkDetail(network);
            networkDetail.Text = detail is null ? "" : Strings.Text(detail);
            networkDetail.Visibility = detail is null ? Visibility.Collapsed : Visibility.Visible;
        }
        else
        {
            networkIcon.Visibility = Visibility.Collapsed;
            networkDetail.Visibility = Visibility.Collapsed;
        }
        addTo.IsEnabled = Profiles.CanAddNetwork(snapshot);
        addProfile.IsEnabled = connected;

        // Every profile, the one in use marked; rebuilt only when what they show changes.
        var shown = status.Profiles.Select(p => (Profile: p, Line: Profiles.Description(p) is { } line ? Strings.Text(line) : null)).ToList();
        var key = string.Join("|", shown.Select(s => $"{s.Profile.Id},{Strings.Text(Profiles.Name(s.Profile))},{s.Line},{Profiles.IsInUse(snapshot, s.Profile)}"));
        if (key != shownRows)
        {
            shownRows = key;
            rows.Children.Clear();
            foreach (var (p, line) in shown)
            {
                rows.Children.Add(ProfileRow(p.Id, Strings.Text(Profiles.Name(p)), line, Profiles.IsInUse(snapshot, p)));
            }
        }

        // Updated without being sent back: while asking for location
        // access, it shows the user's choice.
        Ui.Set(wifiNames, App.Instance.WifiNames.SwitchOn(snapshot));
        wifiNames.IsEnabled = connected && !App.Instance.WifiNames.Asking;
    }

    /// <summary>The line between Automatic and the profiles: not a choice, and not read out.</summary>
    private static ComboBoxItem Separator()
    {
        var item = new ComboBoxItem
        {
            IsEnabled = false,
            IsTabStop = false,
            MinHeight = 0,
            Padding = new Thickness(0, 4, 0, 4),
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Content = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Height = 1,
                Fill = Brand.System("DividerStrokeColorDefaultBrush"),
                HorizontalAlignment = HorizontalAlignment.Stretch,
            },
        };
        AutomationProperties.SetAccessibilityView(item, AccessibilityView.Raw);
        return item;
    }

    /// <summary>A profile in "All profiles": its name, its networks under it, "In use" when it is, and a chevron; pressed, its page.</summary>
    private Button ProfileRow(string id, string name, string? line, bool current)
    {
        var grid = new Grid { ColumnSpacing = 16 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.Children.Add(new FontIcon { Glyph = Profiles.Glyph, FontSize = 18, VerticalAlignment = VerticalAlignment.Center });
        var words = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        words.Children.Add(new TextBlock { Text = name, TextTrimming = TextTrimming.CharacterEllipsis });
        if (line is not null)
        {
            words.Children.Add(Ui.Secondary(line));
        }
        Grid.SetColumn(words, 1);
        grid.Children.Add(words);
        if (current)
        {
            var badge = Ui.Secondary(Strings.Get(TextKeys.ProfilesInUseBadge));
            badge.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(badge, 2);
            grid.Children.Add(badge);
        }
        var chevron = new FontIcon { Glyph = "", FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
        Grid.SetColumn(chevron, 3);
        grid.Children.Add(chevron);
        var row = new Button
        {
            Content = grid,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Padding = new Thickness(16, 12, 16, 12),
            MinHeight = 64,
            CornerRadius = new CornerRadius(6),
            Background = Brand.System("CardBackgroundFillColorDefaultBrush"),
            BorderBrush = Brand.System("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(1),
        };
        var spoken = new List<string> { name };
        if (line is not null)
        {
            spoken.Add(line);
        }
        if (current)
        {
            spoken.Add(Strings.Get(TextKeys.ProfilesInUseBadge));
        }
        AutomationProperties.SetName(row, string.Join(", ", spoken));
        row.Click += (_, _) => ShowProfile(id);
        return row;
    }

    /// <summary>Profile in use: a profile, or Automatic again.</summary>
    private void ChooseProfile()
    {
        int index = inUse.SelectedIndex;
        if (updating || model is not { } snapshot || index is 1 or < 0)
        {
            return;
        }
        var id = index == 0 ? "" : index - 2 < choiceIds.Count ? choiceIds[index - 2] : null;
        if (id is null || id == Profiles.Chosen(snapshot))
        {
            return;
        }
        App.Instance.SelectProfile(id);
    }

    /// <summary>Add to Profile: the profiles the network can go to, the one listing it checked.</summary>
    private void FillAddTo()
    {
        addToMenu.Items.Clear();
        if (model is not { } snapshot)
        {
            return;
        }
        foreach (var target in Profiles.AddTargets(snapshot))
        {
            var item = new ToggleMenuFlyoutItem { Text = Strings.Text(target.Name), IsChecked = target.Checked };
            var id = target.Id;
            var listed = target.Checked;
            item.Click += (_, _) =>
            {
                // The one listing it already stays checked.
                item.IsChecked = listed;
                if (!listed)
                {
                    App.Instance.AddNetworkToProfile(id);
                }
            };
            addToMenu.Items.Add(item);
        }
    }

    /// <summary>Add Profile: a new one, "New Profile" (or "New Profile 2"…), whose page opens with its name ready to type.</summary>
    private async void AddProfile()
    {
        try
        {
            if (await App.Instance.Model.AddProfileAsync(Strings.Get(TextKeys.ProfileNewName)) is { } id)
            {
                model = App.Instance.Model.Snapshot;
                ShowProfile(id, focusName: true);
            }
        }
        catch (Exception e)
        {
            AppLog.Info($"add profile: {e.Message}");
        }
    }

    /// <summary>
    /// One profile's page. Built once for the profile shown, then updated in
    /// place, so typing in the name keeps the focus.
    /// </summary>
    private sealed class ProfileView
    {
        private readonly TextBox name = new() { MaxLength = 64, Width = 240 };
        /// <summary>Under the name, in red, why a new name was refused; until the name is edited again.</summary>
        private readonly TextBlock nameNote = new()
        {
            Style = Ui.Style("CaptionTextBlockStyle"),
            Foreground = Brand.System("SystemFillColorCriticalBrush"),
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 240,
            Margin = new Thickness(0, 4, 0, 0),
            Visibility = Visibility.Collapsed,
        };
        /// <summary>The field is being set from here, not typed in.</summary>
        private bool settingName;
        private readonly RadioButtons sharing = new() { MaxColumns = 2 };
        private readonly TextBlock pausedHint = Ui.Secondary(Strings.Get(TextKeys.ProfilePausedHint));
        /// <summary>The computers, clipboard and files, faded and off while the profile is paused.</summary>
        private readonly ContentControl pausable = new()
        {
            IsTabStop = false,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
        };
        private readonly StackPanel computersCard = new();
        private readonly Border computers;
        private readonly TextBlock computersHint = Ui.Secondary(Strings.Get(TextKeys.ProfileComputersHint));
        private readonly UIElement computersHeader = Ui.Header(Strings.Get(TextKeys.ProfileComputers));
        private readonly ToggleSwitch clipboard;
        private readonly ToggleSwitch files;
        private readonly StackPanel networks = new();
        private readonly Button remove;
        private Profile? shown;
        private AgentSnapshot? model;
        private bool updating;
        private string shownComputers = "";
        private string shownNetworks = "";

        public ProfileView(string id)
        {
            Id = id;
            AutomationProperties.SetName(name, Strings.Get(TextKeys.ProfileName));
            name.KeyDown += (_, e) =>
            {
                if (e.Key == VirtualKey.Enter)
                {
                    e.Handled = true;
                    Rename();
                }
            };
            name.LostFocus += (_, _) => Rename();
            name.TextChanged += (_, _) =>
            {
                if (!settingName)
                {
                    ClearNameNote();
                }
            };
            AutomationProperties.SetName(sharing, Strings.Get(TextKeys.ProfileSharing));
            sharing.Items.Add(Strings.Get(TextKeys.ProfileShare));
            sharing.Items.Add(Strings.Get(TextKeys.ProfilePause));
            sharing.SelectionChanged += (_, _) =>
            {
                if (!updating && sharing.SelectedIndex >= 0 && shown is { } profile && (sharing.SelectedIndex == 1) != profile.Paused)
                {
                    bool paused = sharing.SelectedIndex == 1;
                    Edit($"pause {paused}", new EditProfile { ProfileId = Id, Paused = paused });
                }
            };
            clipboard = Ui.Switch(Strings.Get(TextKeys.ProfileClipboard), true, on =>
                Edit($"clipboard {on}", new EditProfile { ProfileId = Id, Clipboard = on }));
            files = Ui.Switch(Strings.Get(TextKeys.ProfileFiles), true, on =>
                Edit($"files {on}", new EditProfile { ProfileId = Id, Files = on }));
            remove = Ui.Button(Strings.Get(TextKeys.ProfileRemove), Remove);

            var page = new StackPanel();
            var sharingWords = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
            sharingWords.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.ProfileSharing) });
            sharingWords.Children.Add(pausedHint);
            var top = Ui.Card(
                Ui.Row(Strings.Get(TextKeys.ProfileName), null, NameField()),
                Ui.Row(sharingWords, sharing));
            top.Margin = new Thickness(0, 12, 0, 0);
            page.Children.Add(top);

            var inside = new StackPanel();
            inside.Children.Add(computersHeader);
            computers = Ui.Card(computersCard);
            inside.Children.Add(computers);
            computersHint.Margin = new Thickness(2, 6, 0, 0);
            inside.Children.Add(computersHint);
            var sharedThings = Ui.Card(
                Ui.Row(Strings.Get(TextKeys.ProfileClipboard), null, clipboard),
                Ui.Row(Strings.Get(TextKeys.ProfileFiles), null, files));
            sharedThings.Margin = new Thickness(0, 16, 0, 0);
            inside.Children.Add(sharedThings);
            var clipboardHint = Ui.Secondary(Strings.Get(TextKeys.ProfileClipboardHint));
            clipboardHint.Margin = new Thickness(2, 6, 0, 0);
            inside.Children.Add(clipboardHint);
            pausable.Content = inside;
            page.Children.Add(pausable);

            // Networks stay open to change while the profile is paused.
            page.Children.Add(Ui.Header(Strings.Get(TextKeys.ProfileNetworks)));
            page.Children.Add(Ui.Card(networks));
            remove.Margin = new Thickness(0, 24, 0, 0);
            page.Children.Add(remove);
            // As Ui.Page lays a page out, in the scroller the list page shares.
            page.MaxWidth = 1000;
            page.HorizontalAlignment = HorizontalAlignment.Stretch;
            View = new Border { Child = page, Padding = new Thickness(36, 8, 36, 36) };
        }

        public string Id { get; }

        public Border View { get; }

        public void Invalidate()
        {
            model = null;
            shownComputers = "";
            shownNetworks = "";
        }

        public void FocusName()
        {
            if (name.IsEnabled)
            {
                name.Focus(FocusState.Programmatic);
                name.SelectAll();
            }
        }

        public void Update(AgentSnapshot snapshot, Profile profile)
        {
            if (ReferenceEquals(snapshot, model))
            {
                return;
            }
            model = snapshot;
            shown = profile;
            bool connected = snapshot.Link == AgentLink.Connected;
            bool fixedOne = Profiles.Fixed(profile);

            // The field follows the service, except while the user types in it.
            if (name.FocusState == FocusState.Unfocused)
            {
                SetName(Strings.Text(Profiles.Name(profile)));
            }
            name.IsEnabled = connected && !fixedOne;

            updating = true;
            sharing.SelectedIndex = profile.Paused ? 1 : 0;
            sharing.IsEnabled = connected;
            updating = false;
            pausedHint.Visibility = profile.Paused ? Visibility.Visible : Visibility.Collapsed;
            pausable.IsEnabled = connected && !profile.Paused;
            pausable.Opacity = profile.Paused ? 0.5 : 1;

            ShowComputers(snapshot, profile);
            Ui.Set(clipboard, profile.Clipboard);
            Ui.Set(files, profile.Files);
            ShowNetworks(snapshot, profile);
            remove.IsEnabled = connected && !fixedOne;
        }

        /// <summary>A check box for each paired computer; none, and no section, without any.</summary>
        private void ShowComputers(AgentSnapshot snapshot, Profile profile)
        {
            var list = Profiles.Computers(snapshot, profile);
            var key = string.Join("|", list.Select(c => $"{c.Id},{c.Name},{c.Connect}"));
            var visible = list.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
            computersHeader.Visibility = visible;
            computers.Visibility = visible;
            computersHint.Visibility = visible;
            if (key == shownComputers)
            {
                return;
            }
            shownComputers = key;
            computersCard.Children.Clear();
            foreach (var computer in list)
            {
                if (computersCard.Children.Count != 0)
                {
                    computersCard.Children.Add(Ui.Divider());
                }
                var label = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
                var icon = new FontIcon { Glyph = "", FontSize = 16, VerticalAlignment = VerticalAlignment.Center };
                AutomationProperties.SetAccessibilityView(icon, AccessibilityView.Raw);
                label.Children.Add(icon);
                label.Children.Add(new TextBlock { Text = computer.Name, VerticalAlignment = VerticalAlignment.Center });
                var box = new CheckBox { Content = label, IsChecked = computer.Connect, Margin = new Thickness(16, 6, 16, 6) };
                AutomationProperties.SetName(box, computer.Name);
                var id = computer.Id;
                var connect = computer.Connect;
                box.Click += (_, _) =>
                {
                    bool on = box.IsChecked == true;
                    if (on != connect)
                    {
                        var edit = new EditProfile { ProfileId = Id };
                        edit.Devices.Add(new DeviceInProfile { DeviceId = id, Connect = on });
                        Edit($"connect {id} {on}", edit);
                    }
                };
                computersCard.Children.Add(box);
            }
        }

        /// <summary>
        /// Each network with its × and Add Current Network, or that the
        /// profile has none yet; for Other networks only what it is for.
        /// </summary>
        private void ShowNetworks(AgentSnapshot snapshot, Profile profile)
        {
            bool canAdd = Profiles.CanAddCurrentNetwork(snapshot, profile);
            var key = profile.Kind == ProfileKind.OtherNetworks
                ? "other"
                : $"{canAdd}|{snapshot.Link}|{string.Join("|", profile.Networks.Select(n => $"{n.Id},{n.Name},{n.Kind},{n.Router}"))}";
            if (key == shownNetworks)
            {
                return;
            }
            shownNetworks = key;
            networks.Children.Clear();
            void Add(UIElement row)
            {
                if (networks.Children.Count != 0)
                {
                    networks.Children.Add(Ui.Divider());
                }
                networks.Children.Add(row);
            }
            if (profile.Kind == ProfileKind.OtherNetworks)
            {
                Add(Ui.Row(Strings.Get(TextKeys.ProfileOtherNetworksHint)));
                return;
            }
            bool connected = snapshot.Link == AgentLink.Connected;
            foreach (var network in profile.Networks)
            {
                var title = Strings.Text(Profiles.NetworkTitle(network));
                var detail = Profiles.NetworkDetail(network) is { } line ? Strings.Text(line) : null;
                var drop = new Button
                {
                    Content = new FontIcon { Glyph = "", FontSize = 12 },
                    Style = Ui.Style("SubtleButtonStyle"),
                    IsEnabled = connected,
                };
                AutomationProperties.SetName(drop, Strings.Format(TextKeys.ProfileRemoveNetwork, title));
                var networkId = network.Id;
                drop.Click += (_, _) => App.Instance.Send(
                    "remove network", () => App.Instance.Model.RemoveNetworkAsync(networkId));
                Add(Ui.Row(title, detail, drop, new FontIcon { Glyph = Profiles.NetworkGlyph(network) }));
            }
            if (profile.Networks.Count == 0)
            {
                Add(Ui.Row(Ui.Secondary(Strings.Get(TextKeys.ProfileNoNetworks), caption: false)));
            }
            var addCurrent = Ui.Button(Strings.Get(TextKeys.ProfileAddCurrentNetwork), () => App.Instance.AddNetworkToProfile(Id));
            addCurrent.IsEnabled = canAdd;
            addCurrent.HorizontalAlignment = HorizontalAlignment.Left;
            var addRow = new Grid { Padding = new Thickness(16, 12, 16, 12) };
            addRow.Children.Add(addCurrent);
            Add(addRow);
        }

        private void Rename()
        {
            if (shown is not { } profile || Profiles.Fixed(profile))
            {
                return;
            }
            if (Profiles.Rename(profile, name.Text) is { } wanted)
            {
                SendName(profile.Name, wanted);
            }
            else if (name.Text != profile.Name)
            {
                // Emptied, or only spaces added: the name it has comes back.
                SetName(profile.Name);
            }
        }

        /// <summary>
        /// Renames the profile. Refused, its name comes back, with why under
        /// it when another profile has the name (<see cref="Profiles.RenameRefused"/>).
        /// </summary>
        private async void SendName(string before, string wanted)
        {
            try
            {
                await App.Instance.Model.EditProfileAsync(new EditProfile { ProfileId = Id, Name = wanted });
            }
            catch (Exception e)
            {
                AppLog.Info($"profile {Id}: rename: {e.Message}");
                SetName(before);
                if (e is AgentRefusal refusal && Profiles.RenameRefused(refusal.Code) is { } why)
                {
                    nameNote.Text = Strings.Text(why);
                    nameNote.Visibility = Visibility.Visible;
                }
            }
        }

        /// <summary>The name box and the note under it.</summary>
        private StackPanel NameField()
        {
            var field = new StackPanel();
            field.Children.Add(name);
            field.Children.Add(nameNote);
            return field;
        }

        /// <summary>Puts a name in the field without it counting as an edit.</summary>
        private void SetName(string text)
        {
            if (name.Text == text)
            {
                return;
            }
            settingName = true;
            name.Text = text;
            settingName = false;
        }

        /// <summary>Takes the note under the name away: edited again, or the page left.</summary>
        public void ClearNameNote()
        {
            nameNote.Visibility = Visibility.Collapsed;
            nameNote.Text = "";
        }

        private void Remove()
        {
            if (shown is { } profile && !Profiles.Fixed(profile))
            {
                App.Instance.Send("remove profile", () => App.Instance.Model.RemoveProfileAsync(Id));
            }
        }

        /// <summary>Sends a change; refused, the page shows the service's state again.</summary>
        private void Edit(string what, EditProfile edit) =>
            App.Instance.Send($"profile {Id}: {what}", () => App.Instance.Model.EditProfileAsync(edit));
    }
}
