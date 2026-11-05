using Microsoft.Win32;

namespace InputWeave.Support;

/// <summary>
/// Whether the taskbar is light. The taskbar follows Windows' mode
/// (SystemUsesLightTheme), which can differ from the apps' mode.
/// </summary>
internal static class TaskbarTheme
{
    public static bool IsLight
    {
        get
        {
            try
            {
                using var key = Registry.CurrentUser.OpenSubKey(
                    @"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
                // Missing before Windows 10 1903, whose taskbar is dark.
                return key?.GetValue("SystemUsesLightTheme") is int light && light != 0;
            }
            catch (Exception e) when (e is System.Security.SecurityException or IOException or UnauthorizedAccessException)
            {
                return false;
            }
        }
    }
}
