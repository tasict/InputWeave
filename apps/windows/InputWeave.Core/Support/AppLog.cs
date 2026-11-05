using System.Text;

namespace InputWeave.Support;

/// <summary>
/// The app's own log, <c>%LOCALAPPDATA%\InputWeave\app.log</c> (UTF-8).
/// The background service keeps its own logs; this one only says what the
/// app saw. It never holds pairing codes, file names or clipboard contents.
/// </summary>
public static class AppLog
{
    /// <summary>A log over this size is moved aside to app.log.1 at start.</summary>
    private const long RotateAt = 1024 * 1024;

    private static readonly object Gate = new();
    private static readonly UTF8Encoding Utf8 = new(encoderShouldEmitUTF8Identifier: false);
    private static string? path;

    public static string DefaultPath => System.IO.Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "InputWeave", "app.log");

    public static string? Path
    {
        get
        {
            lock (Gate)
            {
                return path;
            }
        }
    }

    public static void Open(string file)
    {
        lock (Gate)
        {
            try
            {
                Directory.CreateDirectory(System.IO.Path.GetDirectoryName(file)!);
                var info = new FileInfo(file);
                if (info.Exists && info.Length > RotateAt)
                {
                    File.Move(file, file + ".1", overwrite: true);
                }
                path = file;
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                path = null;
            }
        }
    }

    public static void Info(string message) => Write("INFO", message);

    public static void Warn(string message) => Write("WARN", message);

    private static void Write(string level, string message)
    {
        var line = $"{DateTimeOffset.Now:yyyy-MM-ddTHH:mm:ss.fffzzz} {level} {message}{Environment.NewLine}";
        lock (Gate)
        {
            if (path is null)
            {
                return;
            }
            try
            {
                File.AppendAllText(path, line, Utf8);
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
                // Logging must never take the app down.
            }
        }
    }
}
