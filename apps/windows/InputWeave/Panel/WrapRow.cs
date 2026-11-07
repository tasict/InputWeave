using Microsoft.UI.Xaml;
using Windows.Foundation;

namespace InputWeave.Panel;

/// <summary>
/// Lays its children out left to right, centered on their line, starting a
/// new line for a child that no longer fits: the panel's title with the
/// profile capsule after it, which moves under a title too long to share
/// its line. A child as wide as the panel wraps its own text.
/// </summary>
public sealed partial class WrapRow : Microsoft.UI.Xaml.Controls.Panel
{
    /// <summary>The space between children on a line, and between lines.</summary>
    public double Spacing { get; set; } = 8;

    protected override Size MeasureOverride(Size available)
    {
        double width = 0, height = 0, lineWidth = 0, lineHeight = 0;
        foreach (var child in Children)
        {
            child.Measure(available);
            var size = child.DesiredSize;
            if (size.Width == 0 && size.Height == 0)
            {
                continue;
            }
            if (lineWidth > 0 && lineWidth + Spacing + size.Width > available.Width)
            {
                width = Math.Max(width, lineWidth);
                height += lineHeight + Spacing;
                (lineWidth, lineHeight) = (0, 0);
            }
            lineWidth += (lineWidth > 0 ? Spacing : 0) + size.Width;
            lineHeight = Math.Max(lineHeight, size.Height);
        }
        width = Math.Max(width, lineWidth);
        height += lineHeight;
        return new Size(double.IsInfinity(available.Width) ? width : Math.Min(width, available.Width), height);
    }

    protected override Size ArrangeOverride(Size final)
    {
        var line = new List<UIElement>();
        double top = 0, lineWidth = 0, lineHeight = 0;
        void Place()
        {
            double left = 0;
            foreach (var child in line)
            {
                var size = child.DesiredSize;
                child.Arrange(new Rect(left, top + (lineHeight - size.Height) / 2, size.Width, size.Height));
                left += size.Width + Spacing;
            }
            top += lineHeight + Spacing;
            line.Clear();
            (lineWidth, lineHeight) = (0, 0);
        }
        foreach (var child in Children)
        {
            var size = child.DesiredSize;
            if (size.Width == 0 && size.Height == 0)
            {
                child.Arrange(new Rect(0, 0, 0, 0));
                continue;
            }
            if (line.Count > 0 && lineWidth + Spacing + size.Width > final.Width)
            {
                Place();
            }
            lineWidth += (line.Count > 0 ? Spacing : 0) + size.Width;
            lineHeight = Math.Max(lineHeight, size.Height);
            line.Add(child);
        }
        if (line.Count > 0)
        {
            Place();
        }
        return final;
    }
}
