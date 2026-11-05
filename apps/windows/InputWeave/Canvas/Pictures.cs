using System.Runtime.InteropServices.WindowsRuntime;
using InputWeave.Support;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Storage.Streams;

namespace InputWeave.Canvas;

/// <summary>
/// The desktop pictures in the arrangement (<c>Screen.preview_jpeg</c>),
/// decoded once per screen for every canvas. Other computers send these
/// bytes: only those that pass <see cref="DesktopPicture"/> reach the
/// decoder, and one that still fails to decode shows no picture.
/// </summary>
/// <remarks>On the UI thread only: pictures are XAML objects.</remarks>
internal static class Pictures
{
    private static readonly PictureCache<BitmapImage> Cache = new(Decode);

    /// <summary>Raised when a picture turned out not to decode, so canvases draw the tile without it.</summary>
    public static event EventHandler? Changed;

    public static BitmapImage? For(string device, string screen, ReadOnlySpan<byte> data) =>
        Cache.Picture(device, screen, data);

    private static BitmapImage? Decode(byte[] bytes)
    {
        var image = new BitmapImage();
        image.ImageFailed += (_, e) => Failed(image, e.ErrorMessage);
        _ = LoadAsync(image, bytes);
        return image;
    }

    private static async Task LoadAsync(BitmapImage image, byte[] bytes)
    {
        try
        {
            using var stream = new InMemoryRandomAccessStream();
            await stream.WriteAsync(bytes.AsBuffer());
            stream.Seek(0);
            await image.SetSourceAsync(stream);
        }
        catch (Exception e)
        {
            Failed(image, e.Message);
        }
    }

    private static void Failed(BitmapImage image, string why)
    {
        AppLog.Info($"a desktop picture did not decode: {why}");
        Cache.Fail(image);
        Changed?.Invoke(null, EventArgs.Empty);
    }
}
