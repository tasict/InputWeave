using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Input;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.ApplicationModel.DataTransfer;
using Windows.Foundation;
using Windows.System;
using Windows.UI.ViewManagement;
using XamlCanvas = Microsoft.UI.Xaml.Controls.Canvas;
using XamlPath = Microsoft.UI.Xaml.Shapes.Path;

namespace InputWeave.Canvas;

/// <summary>
/// The arrangement: every computer as a
/// group of its screens at their real relative sizes on the dotted canvas,
/// with its desktop picture under a light veil, its name, and the stitched
/// seams where computers meet; the keyboard-and-mouse badge on the one
/// they are on. Interactive, the user drags whole computers, which snap to
/// each other's edges and spring back if they would overlap; arrow keys
/// nudge the selected one and Ctrl+Z undoes a move; its context menu moves
/// the cursor there, renames another computer, wakes one that can be
/// woken, which has a moon after its name, and takes another offline for
/// now or brings it back, which has a network with a slash after its name
/// while it is (only Rename… while this one is). Compact, it is the
/// panel's mini map, where a click moves the cursor. Files dropped on a
/// connected computer go to it, unless files are turned off with it. Beside
/// each name is the shortcut that jumps there, where there is room.
/// </summary>
internal sealed partial class ArrangementCanvas : Grid
{
    /// <summary>Snap distance in view units.</summary>
    private const double SnapDistance = 8;
    /// <summary>Arrow keys move a computer this many view units.</summary>
    private const double Nudge = 10;
    /// <summary>The drag starts once the pointer has moved this far.</summary>
    private const double DragStart = 2;
    private const double OfflineOpacity = 0.55;
    private const double NameInset = 6;
    private const double NameHeight = 18;
    private const double CompactNameInset = 3;
    /// <summary>The least the name keeps before the jump shortcut beside it is left out, on the page and on the mini map.</summary>
    private const double LeastName = 40;
    private const double CompactLeastName = 24;
    private static readonly CanvasSize CompactName = new(30, 14);
    private static readonly TimeSpan SpringTime = TimeSpan.FromMilliseconds(200);
    private static readonly TimeSpan ShakeTime = TimeSpan.FromMilliseconds(300);
    private static readonly TimeSpan SewTime = TimeSpan.FromMilliseconds(240);
    /// <summary>The badge grows to this and back when the service shows where the cursor is.</summary>
    private const double PulseScale = 1.15;
    private static readonly TimeSpan PulseTime = TimeSpan.FromMilliseconds(400);
    private static readonly UISettings Ui = new();

    private readonly XamlPath dots = new() { IsHitTestVisible = false };
    private readonly XamlCanvas layer = new() { IsHitTestVisible = false };
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer ticker;
    private readonly Stack<(string Id, CanvasPoint From)> undo = new();
    /// <summary>When each seam, by its place in the list, started being sewn.</summary>
    private readonly List<DateTime> sewn = [];
    private bool drawnSeams;
    private bool interactive;
    private AgentSnapshot? model;
    private string? selection;
    private Drag? drag;
    private Spring? spring;
    private (string Id, DateTime Start)? shake;
    private bool draggingFiles;
    private string? dropTarget;
    private CanvasGeometry.Fit fit = new(0.1, default);
    /// <summary>The cursor sighting the badge last pulsed for; the first one seen is only remembered.</summary>
    private ulong? pulsedFor;
    private DateTime pulseStart = DateTime.MinValue;
    private Size dotsFor;

    private sealed class Drag(string id, CanvasPoint start, CanvasGeometry.Fit fit, Point pointer)
    {
        public string Id { get; } = id;
        public CanvasPoint Start { get; } = start;
        public CanvasPoint Offset { get; set; } = start;
        public CanvasGeometry.Fit Fit { get; } = fit;
        public Point Pointer { get; } = pointer;
        public bool Moving { get; set; }
        /// <summary>Released and sent to the service; shown where it was dropped until it answers.</summary>
        public bool Committed { get; set; }
    }

    private sealed record Spring(string Id, CanvasPoint From, CanvasPoint To, DateTime Start, CanvasGeometry.Fit Fit);

    public ArrangementCanvas()
    {
        Children.Add(dots);
        Children.Add(layer);
        Background = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        AllowDrop = true;
        ticker = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread().CreateTimer();
        ticker.Interval = TimeSpan.FromMilliseconds(16);
        ticker.Tick += (_, _) => Animate();
        SizeChanged += (_, _) => Draw();
        ActualThemeChanged += (_, _) =>
        {
            dotsFor = default;
            Draw();
        };
        Pictures.Changed += (_, _) => Draw();
        Unloaded += (_, _) => ticker.Stop();

        PointerPressed += OnPointerPressed;
        PointerMoved += OnPointerMoved;
        PointerReleased += OnPointerReleased;
        PointerCaptureLost += (_, _) => CancelDrag();
        RightTapped += OnRightTapped;
        Tapped += OnTapped;
        KeyDown += OnKeyDown;
        DragEnter += OnDragOver;
        DragOver += OnDragOver;
        DragLeave += (_, _) => EndFileDrag();
        Drop += OnDrop;
    }

    /// <summary>The arrangement page's canvas: draggable, with selection. Otherwise the mini map.</summary>
    public bool Interactive
    {
        get => interactive;
        set
        {
            interactive = value;
            IsTabStop = value;
            UseSystemFocusVisuals = value;
        }
    }

    /// <summary>Room around the computers, in view units.</summary>
    public double ContentPadding { get; set; } = 40;

    /// <summary>The selected computer, on the arrangement page.</summary>
    public string? Selection
    {
        get => selection;
        set
        {
            if (selection == value)
            {
                return;
            }
            selection = value;
            SelectionChanged?.Invoke(this, EventArgs.Empty);
            Draw();
        }
    }

    public event EventHandler? SelectionChanged;

    /// <summary>"Rename…" was chosen for a computer, now selected: its name field is to take the focus.</summary>
    public event EventHandler<string>? RenameRequested;

    /// <summary>Shows what the service reports now.</summary>
    public void Update(AgentSnapshot snapshot)
    {
        if (ReferenceEquals(snapshot, model))
        {
            return;
        }
        model = snapshot;
        var sighting = snapshot.CursorSighting?.Serial ?? 0;
        if (pulsedFor is null)
        {
            pulsedFor = sighting;
        }
        else if (sighting != pulsedFor)
        {
            pulsedFor = sighting;
            if (!ReduceMotion)
            {
                pulseStart = DateTime.UtcNow;
                ticker.Start();
            }
        }
        Draw();
    }

    private static bool ReduceMotion => !Ui.AnimationsEnabled;

    // Drawing

    /// <summary>The computers as they are now, with the one being dragged or springing back where it is on screen.</summary>
    private IReadOnlyList<CanvasDevice> Devices()
    {
        if (model is null)
        {
            return [];
        }
        return CanvasGeometry.Devices(model.Arrangement)
            .Select(d => drag is { } g && g.Id == d.Id ? d.MovedTo(g.Offset)
                : spring is { } s && s.Id == d.Id ? d.MovedTo(SpringOffset(s))
                : d)
            .ToList();
    }

    private void Draw()
    {
        var size = new CanvasSize(ActualWidth, ActualHeight);
        if (size.Width <= 0 || size.Height <= 0)
        {
            return;
        }
        var brand = Brand.For(this);
        DrawDots(brand);
        layer.Children.Clear();
        if (model is not { Link: AgentLink.Connected } current)
        {
            return;
        }
        var devices = Devices();
        if (CanvasGeometry.Union(devices.SelectMany(d => d.Frames)) is not { } content)
        {
            return;
        }
        fit = drag?.Fit ?? spring?.Fit ?? CanvasGeometry.FitInto(content, size, ContentPadding);

        foreach (var device in devices)
        {
            layer.Children.Add(DeviceGroup(current, device, devices, brand));
        }
        // On top: the stitches cross both screens at the seam.
        // A seam is sewn shut when it appears, as when a computer snaps
        // into place; those there at the first drawing are simply there.
        var seams = CanvasGeometry.Seams(devices);
        var now = DateTime.UtcNow;
        if (sewn.Count > seams.Count)
        {
            sewn.RemoveRange(seams.Count, sewn.Count - seams.Count);
        }
        while (sewn.Count < seams.Count)
        {
            sewn.Add(drawnSeams && !ReduceMotion ? now : DateTime.MinValue);
        }
        drawnSeams = true;
        for (int i = 0; i < seams.Count; i++)
        {
            layer.Children.Add(Stitches(seams[i], brand, Math.Clamp((now - sewn[i]) / SewTime, 0, 1)));
        }
        if (Sewing)
        {
            ticker.Start();
        }
        // Above the seams too, so nothing covers it.
        if (!draggingFiles && Badge(current, devices, brand) is { } badge)
        {
            layer.Children.Add(badge);
        }
    }

    /// <summary>The canvas's dot grid, redrawn only when its size or theme changes.</summary>
    private void DrawDots(Brand brand)
    {
        var size = new Size(ActualWidth, ActualHeight);
        if (size == dotsFor)
        {
            return;
        }
        dotsFor = size;
        double spacing = Interactive ? 16 : 10;
        var group = new GeometryGroup();
        for (double y = spacing / 2; y < size.Height; y += spacing)
        {
            for (double x = spacing / 2; x < size.Width; x += spacing)
            {
                group.Children.Add(new EllipseGeometry { Center = new Point(x, y), RadiusX = 1, RadiusY = 1 });
            }
        }
        dots.Data = group;
        dots.Fill = brand.Grid;
    }

    /// <summary>One computer: an outline around its screens, the screens as tiles, and its name.</summary>
    private FrameworkElement DeviceGroup(AgentSnapshot current, CanvasDevice device, IReadOnlyList<CanvasDevice> all, Brand brand)
    {
        var info = current.Arranged(device.Id);
        var bounds = fit.ToView(device.Bounds);
        bool online = info is { Connected: true } or { ThisDevice: true };
        bool selected = Interactive && selection == device.Id;
        bool active = current.ControllingOther && current.Status.ActiveDeviceId == device.Id;
        var name = info?.Name ?? device.Id;

        var group = new XamlCanvas { Width = bounds.Width, Height = bounds.Height };
        XamlCanvas.SetLeft(group, bounds.X);
        XamlCanvas.SetTop(group, bounds.Y);

        var content = new XamlCanvas { Opacity = online ? 1 : OfflineOpacity };
        group.Children.Add(content);
        var tiles = device.Frames.Select(f => fit.ToView(f).Offset(-bounds.X, -bounds.Y)).ToList();
        for (int i = 0; i < tiles.Count; i++)
        {
            var screen = info is not null && i < info.Screens.Count ? info.Screens[i] : null;
            content.Children.Add(Tile(device.Id, screen, tiles[i], brand));
        }
        var jump = current.JumpShortcut(info?.JumpSlot ?? 0);
        bool screenLocked = DeviceText.ScreenLocked(current, device.Id);
        bool offlineForNow = OfflineForNow.IsOffline(current, device.Id);
        // By the status's order: offline for now, a locked screen (only
        // while connected), or a computer that can be woken (only offline).
        string? glyph = offlineForNow ? OfflineForNow.Glyph
            : screenLocked ? DeviceText.LockedGlyph
            : Wake.Offered(current, device.Id) ? Wake.MoonGlyph
            : null;
        if (NameLabel(name, jump, glyph, screenLocked, tiles, device.MainScreen) is { } label)
        {
            content.Children.Add(label);
        }

        // The outline: selected in the accent color, the computer being
        // controlled in the thread color, offline dashed.
        if (selected || active || !online)
        {
            var outline = new Rectangle
            {
                Width = bounds.Width + 6,
                Height = bounds.Height + 6,
                RadiusX = 6,
                RadiusY = 6,
                Stroke = selected ? Brand.System("AccentFillColorDefaultBrush") : active ? brand.Thread : brand.TileEdge,
                StrokeThickness = selected || active ? 2 : 1,
            };
            if (!selected && !active)
            {
                outline.StrokeDashArray = new DoubleCollection { 4, 3 };
            }
            XamlCanvas.SetLeft(outline, -3);
            XamlCanvas.SetTop(outline, -3);
            group.Children.Add(outline);
        }

        // While files are dragged over the canvas, the computers say whether they take them.
        if (draggingFiles)
        {
            bool receives = CanReceive(device.Id);
            bool target = dropTarget == device.Id;
            if (!receives)
            {
                group.Opacity = 0.35;
            }
            if (target)
            {
                group.RenderTransform = new ScaleTransform
                {
                    ScaleX = 1.06, ScaleY = 1.06, CenterX = bounds.Width / 2, CenterY = bounds.Height / 2,
                };
                var ring = new Rectangle
                {
                    Width = bounds.Width + 6, Height = bounds.Height + 6, RadiusX = 6, RadiusY = 6,
                    Stroke = Brand.System("AccentFillColorDefaultBrush"), StrokeThickness = 2,
                };
                XamlCanvas.SetLeft(ring, -3);
                XamlCanvas.SetTop(ring, -3);
                group.Children.Add(ring);
            }
            var why = receives ? Strings.Get(TextKeys.CanvasDropToSend)
                : info?.ThisDevice == true ? null
                : DeviceText.CannotReceive(current, device.Id, compact: !Interactive) is { } cannot ? Strings.Text(cannot)
                : null;
            if (why is not null)
            {
                var capsule = Capsule(new TextBlock
                {
                    Text = why,
                    FontSize = Interactive ? 12 : 10,
                    FontWeight = FontWeights.SemiBold,
                    MaxLines = 1,
                    TextTrimming = TextTrimming.CharacterEllipsis,
                }, Interactive ? 20 : 14, Interactive ? 6 : 4);
                capsule.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
                XamlCanvas.SetLeft(capsule, (bounds.Width - capsule.DesiredSize.Width) / 2);
                XamlCanvas.SetTop(capsule, (bounds.Height - capsule.DesiredSize.Height) / 2);
                group.Children.Add(capsule);
            }
        }
        else if (shake is { } s && s.Id == device.Id)
        {
            group.RenderTransform = new TranslateTransform { X = ShakeOffset(s.Start) };
        }

        var neighbours = CanvasGeometry.Neighbours(device, all).Select(current.NameOf).ToList();
        AutomationProperties.SetName(group, Strings.Text(DeviceText.CanvasLabel(name, device.Screens.Count, neighbours)));
        if (jump is not null)
        {
            AutomationProperties.SetAcceleratorKey(group, jump);
        }
        bool here = current.CursorDeviceId == device.Id;
        string? status = (screenLocked, here) switch
        {
            _ when offlineForNow => Strings.Get(TextKeys.ConnectionOfflineForNow),
            (true, _) when current.OutOfReach(device.Id) => Strings.Get(TextKeys.ConnectionScreenLockedNoInput),
            (false, _) when current.OutOfReach(device.Id) => Strings.Get(TextKeys.ConnectionNoInput),
            (true, true) => Strings.Get(TextKeys.CanvasScreenLockedKeyboardMouseHere),
            (true, false) => Strings.Get(TextKeys.ConnectionScreenLocked),
            (false, true) => Strings.Get(TextKeys.CanvasKeyboardMouseHere),
            _ => null,
        };
        if (status is not null)
        {
            AutomationProperties.SetItemStatus(group, status);
        }
        return group;
    }

    /// <summary>
    /// A screen: the tile color, or the computer's desktop picture filling
    /// it under a veil of the tile color that keeps names, seams and the
    /// selection clear; then its edge, and on the arrangement page its
    /// resolution.
    /// </summary>
    private FrameworkElement Tile(string deviceId, Screen? screen, CanvasRect rect, Brand brand)
    {
        var tile = new Grid { Width = Math.Max(rect.Width, 1), Height = Math.Max(rect.Height, 1) };
        XamlCanvas.SetLeft(tile, rect.X);
        XamlCanvas.SetTop(tile, rect.Y);
        var corner = new CornerRadius(Interactive ? 3 : 2);
        tile.Children.Add(new Border { Background = brand.Tile, CornerRadius = corner });
        var picture = screen is null || brand.HighContrast ? null : Pictures.For(deviceId, screen.Id, screen.PreviewJpeg.Span);
        if (picture is not null)
        {
            // The whole picture is sent, whatever the screen's shape: it fills the screen and is cropped to it.
            tile.Children.Add(new Border
            {
                Background = new ImageBrush { ImageSource = picture, Stretch = Stretch.UniformToFill },
                CornerRadius = corner,
            });
            tile.Children.Add(new Border { Background = brand.Tile, Opacity = brand.Dark ? 0.35 : 0.25, CornerRadius = corner });
        }
        tile.Children.Add(new Border { BorderBrush = brand.TileEdge, BorderThickness = new Thickness(1), CornerRadius = corner });

        if (Interactive && screen is not null && rect.Width > 90)
        {
            var detail = new StackPanel { HorizontalAlignment = HorizontalAlignment.Right, Spacing = 0 };
            var secondary = Brand.System("TextFillColorSecondaryBrush");
            if (screen.Primary)
            {
                detail.Children.Add(new TextBlock
                {
                    Text = Strings.Get(TextKeys.ScreenMain),
                    FontSize = 11,
                    FontWeight = FontWeights.SemiBold,
                    Foreground = secondary,
                    HorizontalAlignment = HorizontalAlignment.Right,
                });
            }
            detail.Children.Add(new TextBlock
            {
                Text = DeviceText.Resolution(screen),
                FontSize = 11,
                Foreground = secondary,
                HorizontalAlignment = HorizontalAlignment.Right,
            });
            FrameworkElement placed = picture is null
                ? new Border { Child = detail, Padding = new Thickness(5) }
                // On a picture, on material of its own to stay readable.
                : new Border
                {
                    Child = detail,
                    Padding = new Thickness(4, 2, 4, 2),
                    Margin = new Thickness(3),
                    CornerRadius = new CornerRadius(4),
                    Background = Material,
                };
            placed.HorizontalAlignment = HorizontalAlignment.Right;
            placed.VerticalAlignment = VerticalAlignment.Bottom;
            tile.Children.Add(placed);
        }
        return tile;
    }

    /// <summary>
    /// The computer's name, <paramref name="glyph"/> after it (a network
    /// with a slash while it is offline for now, a padlock while its screen
    /// is locked or a moon while it can be woken), and,
    /// beside them in the secondary color, the shortcut that jumps there
    /// (<paramref name="jump"/>, when it has one): on the arrangement page
    /// with a display glyph in the main screen's corner; on the mini map
    /// small, on the main screen or the widest that has room, or not at all. The shortcut is left out where
    /// the name would have too little room (<see cref="CanvasGeometry.NameBeside"/>).
    /// </summary>
    private FrameworkElement? NameLabel(
        string name, string? jump, string? glyph, bool screenLocked, IReadOnlyList<CanvasRect> tiles, int main)
    {
        int index;
        if (Interactive)
        {
            if (main >= tiles.Count)
            {
                return null;
            }
            index = main;
        }
        else if (CompactTile(tiles, main) is { } compactTile)
        {
            index = compactTile;
        }
        else
        {
            return null;
        }
        var tile = tiles[index];
        double fontSize = Interactive ? 12 : 10;
        double padding = Interactive ? 6 : 4;
        double inset = Interactive ? NameInset : CompactNameInset;
        double spacing = Interactive ? 5 : 3;
        double labelWidth = Interactive ? Math.Max(tile.Width - 2 * inset, 40) : tile.Width - 2 * inset;
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = spacing };
        double room = labelWidth - 2 * padding;
        if (Interactive)
        {
            row.Children.Add(new FontIcon { Glyph = "", FontSize = 12, VerticalAlignment = VerticalAlignment.Center });
            room -= 12 + spacing;
        }
        var nameText = new TextBlock
        {
            Text = name,
            FontSize = fontSize,
            FontWeight = FontWeights.SemiBold,
            MaxLines = 1,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
        };
        row.Children.Add(nameText);
        // On a narrow mini map tile the name keeps its least width and the
        // glyph is left out.
        if (glyph is not null && (Interactive || room - (fontSize + spacing) >= CompactLeastName))
        {
            var icon = new FontIcon
            {
                Glyph = glyph,
                FontSize = fontSize,
                VerticalAlignment = VerticalAlignment.Center,
            };
            if (!screenLocked)
            {
                // The status says it is offline for now or can be woken;
                // the glyph only shows it.
                AutomationProperties.SetAccessibilityView(icon, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
            }
            row.Children.Add(icon);
            room -= fontSize + spacing;
        }
        TextBlock? shortcut = null;
        if (jump is not null)
        {
            shortcut = new TextBlock
            {
                Text = jump,
                FontSize = fontSize,
                FontWeight = FontWeights.SemiBold,
                MaxLines = 1,
                Foreground = Brand.System("TextFillColorSecondaryBrush"),
                VerticalAlignment = VerticalAlignment.Center,
            };
            shortcut.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        }
        var (showsShortcut, nameWidth) = CanvasGeometry.NameBeside(
            room, shortcut?.DesiredSize.Width ?? 0, spacing, Interactive ? LeastName : CompactLeastName);
        nameText.MaxWidth = Math.Max(nameWidth, 1);
        if (showsShortcut && shortcut is not null)
        {
            row.Children.Add(shortcut);
        }
        var label = Capsule(row, Interactive ? NameHeight : CompactName.Height, padding);
        label.MaxWidth = labelWidth;
        XamlCanvas.SetLeft(label, tile.X + inset);
        XamlCanvas.SetTop(label, tile.Y + inset);
        return label;
    }

    private static int? CompactTile(IReadOnlyList<CanvasRect> tiles, int main) =>
        CanvasGeometry.LabelTile(
            tiles, main, new CanvasSize(CompactName.Width + 2 * CompactNameInset, CompactName.Height + 2 * CompactNameInset));

    /// <summary>Where the name on the main screen's tile ends, or null when it is not there.</summary>
    private double? NameBottom(IReadOnlyList<CanvasRect> tiles, int main)
    {
        if (main >= tiles.Count)
        {
            return null;
        }
        if (Interactive)
        {
            return tiles[main].MinY + NameInset + NameHeight;
        }
        return CompactTile(tiles, main) == main ? tiles[main].MinY + CompactNameInset + CompactName.Height : null;
    }

    /// <summary>
    /// The keyboard and mouse on the main screen of the computer they are
    /// on, saying so in words on the arrangement page when the screen has
    /// room, clear of the name.
    /// </summary>
    private FrameworkElement? Badge(AgentSnapshot current, IReadOnlyList<CanvasDevice> devices, Brand brand)
    {
        var id = current.CursorDeviceId;
        if (devices.FirstOrDefault(d => d.Id == id) is not { } device || device.MainScreen >= device.Screens.Count)
        {
            return null;
        }
        var tiles = device.Frames.Select(fit.ToView).ToList();
        var tile = tiles[device.MainScreen];
        double inset = Interactive ? NameInset : 2;
        var badge = BadgeCapsule(brand, words: Interactive);
        badge.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        if (Interactive && badge.DesiredSize.Width > tile.Width - 2 * inset)
        {
            badge = BadgeCapsule(brand, words: false);
            badge.Measure(new Size(double.PositiveInfinity, double.PositiveInfinity));
        }
        var frame = CanvasGeometry.BadgeFrame(
            new CanvasSize(badge.DesiredSize.Width, badge.DesiredSize.Height), tile, NameBottom(tiles, device.MainScreen), inset);
        double x = frame.X;
        if (shake is { } s && s.Id == id)
        {
            x += ShakeOffset(s.Start);
        }
        XamlCanvas.SetLeft(badge, x);
        XamlCanvas.SetTop(badge, frame.Y);
        if (Pulse() is var grown && grown != 1)
        {
            badge.RenderTransform = new ScaleTransform
            {
                ScaleX = grown,
                ScaleY = grown,
                CenterX = badge.DesiredSize.Width / 2,
                CenterY = badge.DesiredSize.Height / 2,
            };
        }
        AutomationProperties.SetAccessibilityView(badge, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        return badge;
    }

    private Border BadgeCapsule(Brand brand, bool words)
    {
        bool compact = !Interactive;
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = compact ? 2 : 4 };
        double glyph = compact ? 9 : 12;
        row.Children.Add(new FontIcon { Glyph = "", FontSize = glyph, VerticalAlignment = VerticalAlignment.Center });
        row.Children.Add(new FontIcon { Glyph = "", FontSize = glyph, VerticalAlignment = VerticalAlignment.Center });
        if (words)
        {
            row.Children.Add(new TextBlock
            {
                Text = Strings.Get(TextKeys.CanvasKeyboardMouseHere),
                FontSize = 12,
                FontWeight = FontWeights.SemiBold,
                MaxLines = 1,
                VerticalAlignment = VerticalAlignment.Center,
            });
        }
        var badge = Capsule(row, compact ? 14 : 20, compact ? 4 : 7);
        badge.BorderBrush = brand.TileEdge;
        badge.BorderThickness = new Thickness(1);
        return badge;
    }

    /// <summary>The material under names and badges, so they read on any desktop picture.</summary>
    private static Brush Material => Brand.System("AcrylicInAppFillColorDefaultBrush");

    private static Border Capsule(UIElement child, double height, double padding) => new()
    {
        Child = child,
        Height = height,
        Padding = new Thickness(padding, 0, padding, 0),
        CornerRadius = new CornerRadius(height / 2),
        Background = Material,
        IsHitTestVisible = false,
    };

    /// <summary>The thread where two computers meet: stitches crossing the seam; solid and thicker in high contrast.</summary>
    private Polyline Stitches(Seam seam, Brand brand, double progress)
    {
        var start = fit.ToView(seam.Start);
        var end = fit.ToView(seam.End);
        var line = new Polyline
        {
            Stroke = brand.Thread,
            StrokeThickness = brand.HighContrast ? 3 : Interactive ? 2 : 1.5,
            StrokeLineJoin = PenLineJoin.Round,
            StrokeStartLineCap = PenLineCap.Round,
            StrokeEndLineCap = PenLineCap.Round,
            IsHitTestVisible = false,
        };
        IReadOnlyList<CanvasPoint> points = brand.HighContrast
            ? [start, end]
            : CanvasGeometry.Stitch(start, end, pitch: Interactive ? 6 : 4, reach: Interactive ? 3.5 : 2);
        foreach (var p in Sewn(points, progress))
        {
            line.Points.Add(new Point(p.X, p.Y));
        }
        return line;
    }

    /// <summary>The first <paramref name="progress"/> of a line through <paramref name="points"/>.</summary>
    private static IEnumerable<CanvasPoint> Sewn(IReadOnlyList<CanvasPoint> points, double progress)
    {
        if (progress >= 1 || points.Count < 2)
        {
            return points;
        }
        double total = 0;
        for (int i = 1; i < points.Count; i++)
        {
            total += Distance(points[i - 1], points[i]);
        }
        var left = total * progress;
        var sewn = new List<CanvasPoint> { points[0] };
        for (int i = 1; i < points.Count && left > 0; i++)
        {
            var step = Distance(points[i - 1], points[i]);
            if (step <= left)
            {
                sewn.Add(points[i]);
                left -= step;
            }
            else
            {
                var t = left / step;
                sewn.Add(new CanvasPoint(
                    points[i - 1].X + (points[i].X - points[i - 1].X) * t,
                    points[i - 1].Y + (points[i].Y - points[i - 1].Y) * t));
                left = 0;
            }
        }
        return sewn;
    }

    private static double Distance(CanvasPoint a, CanvasPoint b) => Math.Sqrt(Math.Pow(b.X - a.X, 2) + Math.Pow(b.Y - a.Y, 2));

    // Animation: the spring back, the shake of a refused move, a seam being sewn.

    private CanvasPoint SpringOffset(Spring s)
    {
        var t = Math.Clamp((DateTime.UtcNow - s.Start) / SpringTime, 0, 1);
        var eased = 1 - Math.Pow(1 - t, 3);
        return new CanvasPoint(s.From.X + (s.To.X - s.From.X) * eased, s.From.Y + (s.To.Y - s.From.Y) * eased);
    }

    private static double ShakeOffset(DateTime start)
    {
        var amount = Math.Clamp((DateTime.UtcNow - start) / ShakeTime, 0, 1);
        return 6 * Math.Sin(amount * Math.PI * 4);
    }

    /// <summary>The badge's scale now: up to <see cref="PulseScale"/> and back over <see cref="PulseTime"/>.</summary>
    private double Pulse()
    {
        var t = (DateTime.UtcNow - pulseStart) / PulseTime;
        if (t is < 0 or >= 1)
        {
            return 1;
        }
        var half = t < 0.5 ? t * 2 : (1 - t) * 2;
        var eased = 1 - Math.Pow(1 - half, 3);
        return 1 + (PulseScale - 1) * eased;
    }

    private bool Pulsing => DateTime.UtcNow - pulseStart < PulseTime;

    private void Animate()
    {
        var now = DateTime.UtcNow;
        if (spring is { } s && now - s.Start > SpringTime)
        {
            spring = null;
        }
        if (shake is { } k && now - k.Start > ShakeTime)
        {
            shake = null;
        }
        if (spring is null && shake is null && !Sewing && !Pulsing)
        {
            ticker.Stop();
        }
        Draw();
    }

    private bool Sewing => sewn.Any(start => DateTime.UtcNow - start < SewTime);

    private void SpringBack(string id, CanvasPoint from, CanvasPoint to, CanvasGeometry.Fit at)
    {
        drag = null;
        if (ReduceMotion || from == to)
        {
            spring = null;
            Draw();
            return;
        }
        spring = new Spring(id, from, to, DateTime.UtcNow, at);
        ticker.Start();
        Draw();
    }

    /// <summary>A short horizontal shake, for a refused move or jump.</summary>
    private void Flag(string id)
    {
        if (ReduceMotion)
        {
            return;
        }
        shake = (id, DateTime.UtcNow);
        ticker.Start();
    }

    // Moving computers

    private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        var point = e.GetCurrentPoint(this);
        if (!Interactive || !point.Properties.IsLeftButtonPressed || model is null)
        {
            return;
        }
        Focus(FocusState.Pointer);
        var devices = Devices();
        if (HitTest(point.Position, 0) is not { } id || devices.FirstOrDefault(d => d.Id == id) is not { } device)
        {
            return;
        }
        Selection = id;
        if (drag is { Committed: true } || spring is not null)
        {
            return;
        }
        drag = new Drag(id, device.Offset, fit, point.Position);
        CapturePointer(e.Pointer);
        e.Handled = true;
    }

    private void OnPointerMoved(object sender, PointerRoutedEventArgs e)
    {
        if (drag is not { Committed: false } current)
        {
            return;
        }
        var position = e.GetCurrentPoint(this).Position;
        var (dx, dy) = (position.X - current.Pointer.X, position.Y - current.Pointer.Y);
        if (!current.Moving && Math.Abs(dx) < DragStart && Math.Abs(dy) < DragStart)
        {
            return;
        }
        current.Moving = true;
        var all = CanvasGeometry.Devices(model!.Arrangement);
        if (all.FirstOrDefault(d => d.Id == current.Id) is not { } device)
        {
            return;
        }
        var scale = current.Fit.Scale;
        var moving = device.MovedTo(new CanvasPoint(current.Start.X + dx / scale, current.Start.Y + dy / scale));
        current.Offset = CanvasGeometry.Snap(moving, all.Where(d => d.Id != current.Id), SnapDistance / scale);
        Draw();
        e.Handled = true;
    }

    private void OnPointerReleased(object sender, PointerRoutedEventArgs e)
    {
        if (drag is not { Committed: false } current)
        {
            return;
        }
        ReleasePointerCapture(e.Pointer);
        if (!current.Moving)
        {
            drag = null;
            return;
        }
        _ = CommitAsync(current.Id, current.Start, current.Offset, current.Fit);
        e.Handled = true;
    }

    private void CancelDrag()
    {
        if (drag is { Committed: false, Moving: true } current)
        {
            SpringBack(current.Id, current.Offset, current.Start, current.Fit);
        }
        else if (drag is { Committed: false })
        {
            drag = null;
        }
    }

    /// <summary>
    /// Applies a move unless it makes screens overlap, in which case the
    /// computer springs back. The service shares the change with every
    /// connected computer.
    /// </summary>
    private async Task CommitAsync(string id, CanvasPoint from, CanvasPoint to, CanvasGeometry.Fit at, bool undoing = false)
    {
        if (model is null || CanvasGeometry.Devices(model.Arrangement).FirstOrDefault(d => d.Id == id) is not { } device)
        {
            drag = null;
            Draw();
            return;
        }
        var others = CanvasGeometry.Devices(model.Arrangement).Where(d => d.Id != id).SelectMany(d => d.Frames).ToList();
        if (from == to)
        {
            drag = null;
            Draw();
            return;
        }
        if (CanvasGeometry.Overlaps(device.MovedTo(to).Frames, others))
        {
            SpringBack(id, to, from, at);
            Flag(id);
            return;
        }
        // Shown where it was dropped until the service answers.
        drag = new Drag(id, from, at, default) { Offset = to, Moving = true, Committed = true };
        Draw();
        try
        {
            await App.Instance.Model.MoveAsync([(id, to.X, to.Y)]);
            if (!undoing)
            {
                undo.Push((id, from));
            }
            drag = null;
            Draw();
        }
        catch (Exception e)
        {
            AppLog.Info($"move {id}: {e.Message}");
            SpringBack(id, to, from, at);
            Flag(id);
        }
    }

    private void OnKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (!Interactive || model is null)
        {
            return;
        }
        var ctrl = InputKeyboardSource.GetKeyStateForCurrentThread(VirtualKey.Control).HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down);
        if (ctrl && e.Key == VirtualKey.Z)
        {
            e.Handled = true;
            if (drag is null && spring is null && undo.TryPop(out var last)
                && CanvasGeometry.Devices(model.Arrangement).FirstOrDefault(d => d.Id == last.Id) is { } moved)
            {
                _ = CommitAsync(last.Id, moved.Offset, last.From, fit, undoing: true);
            }
            return;
        }
        var step = Nudge / fit.Scale;
        (double X, double Y)? delta = e.Key switch
        {
            VirtualKey.Left => (-step, 0),
            VirtualKey.Right => (step, 0),
            VirtualKey.Up => (0, -step),
            VirtualKey.Down => (0, step),
            _ => null,
        };
        if (delta is not { } d || selection is not { } id || drag is not null || spring is not null)
        {
            return;
        }
        e.Handled = true;
        if (CanvasGeometry.Devices(model.Arrangement).FirstOrDefault(x => x.Id == id) is { } device)
        {
            _ = CommitAsync(id, device.Offset, device.Offset.Offset(d.X, d.Y), fit);
        }
    }

    // Moving the cursor

    private void OnRightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        if (!Interactive || model is null || HitTest(e.GetPosition(this), 0) is not { } id)
        {
            return;
        }
        e.Handled = true;
        Selection = id;
        var menu = new MenuFlyout();
        // While this computer is offline for now, nothing but a name can be
        // given to the others.
        if (!OfflineForNow.OnlyRename(model, id))
        {
            var item = new MenuFlyoutItem
            {
                Text = Strings.Get(TextKeys.CanvasMoveCursorHere),
                Icon = new FontIcon { Glyph = "" },
                IsEnabled = model.CanJump(id),
            };
            item.Click += (_, _) => Jump(id);
            menu.Items.Add(item);
        }
        // Another computer this one trusts can be given a name here; this
        // one is renamed in General.
        if (id != model.ThisDeviceId && model.Device(id) is not null)
        {
            var rename = new MenuFlyoutItem { Text = Strings.Get(TextKeys.CanvasRename), Icon = new FontIcon { Glyph = "" } };
            rename.Click += (_, _) => RenameRequested?.Invoke(this, id);
            menu.Items.Add(rename);
        }
        // After a line: Wake for one that can be woken, unless it is being
        // woken or a wake-up for it is on its way; then Take Offline or
        // Bring Back Online.
        bool wakes = Wake.Offered(model, id);
        bool offline = OfflineForNow.Offered(model, id);
        if (wakes || offline)
        {
            menu.Items.Add(new MenuFlyoutSeparator());
        }
        if (wakes)
        {
            var wake = new MenuFlyoutItem
            {
                Text = Strings.Get(TextKeys.WakeAction),
                Icon = new FontIcon { Glyph = Wake.MoonGlyph },
                IsEnabled = Wake.CanSend(model, id),
            };
            wake.Click += (_, _) => App.Instance.WakeDevice(id);
            menu.Items.Add(wake);
        }
        if (offline)
        {
            bool bringBack = OfflineForNow.IsOffline(model, id);
            var take = new MenuFlyoutItem
            {
                Text = Strings.Get(OfflineForNow.ActionKey(model, id)),
                Icon = new FontIcon { Glyph = OfflineForNow.Glyph },
            };
            take.Click += (_, _) => App.Instance.SetOfflineForNow(id, !bringBack);
            menu.Items.Add(take);
        }
        if (menu.Items.Count != 0)
        {
            menu.ShowAt(this, e.GetPosition(this));
        }
    }

    private void OnTapped(object sender, TappedRoutedEventArgs e)
    {
        // On the mini map, a click moves the cursor there; nothing happens
        // where it cannot go, so the pointer never jumps off the panel for nothing.
        if (Interactive || model is null || HitTest(e.GetPosition(this), 2) is not { } id || !model.CanJump(id))
        {
            return;
        }
        e.Handled = true;
        Jump(id);
    }

    /// <summary>
    /// Moves the cursor to a computer. Refusals the panel already explains
    /// (sharing paused, cursor locked) are left at that; others shake it.
    /// </summary>
    private async void Jump(string id)
    {
        try
        {
            await App.Instance.Model.JumpToAsync(id);
        }
        catch (AgentRefusal refusal) when (refusal.Code is "sharing_paused" or "locked")
        {
        }
        catch (Exception e)
        {
            AppLog.Info($"jump to {id}: {e.Message}");
            Flag(id);
        }
    }

    // Sending files

    /// <summary>Connected, and files not turned off with it here.</summary>
    private bool CanReceive(string id) => model is not null && model.CanReceiveFiles(id);

    private void OnDragOver(object sender, DragEventArgs e)
    {
        if (model is not { Link: AgentLink.Connected } || !e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = DataPackageOperation.None;
            return;
        }
        var target = HitTest(e.GetPosition(this), 4) is { } id && CanReceive(id) ? id : null;
        e.AcceptedOperation = target is null ? DataPackageOperation.None : DataPackageOperation.Copy;
        if (target is not null)
        {
            e.DragUIOverride.Caption = model.NameOf(target);
        }
        if (!draggingFiles || target != dropTarget)
        {
            draggingFiles = true;
            dropTarget = target;
            Draw();
        }
    }

    private void EndFileDrag()
    {
        draggingFiles = false;
        dropTarget = null;
        Draw();
    }

    private async void OnDrop(object sender, DragEventArgs e)
    {
        var target = dropTarget;
        EndFileDrag();
        if (target is null || !e.DataView.Contains(StandardDataFormats.StorageItems))
        {
            return;
        }
        var deferral = e.GetDeferral();
        List<string> paths;
        try
        {
            var items = await e.DataView.GetStorageItemsAsync();
            paths = items.Select(i => i.Path).Where(p => !string.IsNullOrEmpty(p)).ToList();
        }
        finally
        {
            deferral.Complete();
        }
        if (paths.Count == 0)
        {
            return;
        }
        try
        {
            await App.Instance.Model.SendFilesAsync(target, paths);
        }
        catch (Exception error)
        {
            AppLog.Info($"send files to {target}: {error.Message}");
            Flag(target);
            Support.SendFiles.ShowFailure(error, App.Instance.Model.Snapshot.NameOf(target));
        }
    }

    private string? HitTest(Point position, double slop) =>
        CanvasGeometry.HitTest(Devices(), fit, new CanvasPoint(position.X, position.Y), slop);
}
