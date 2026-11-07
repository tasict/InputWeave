namespace InputWeave.Main;

/// <summary>The main window's pages, in the order its sidebar lists them; it opens on the first.</summary>
public enum MainPage
{
    General,
    /// <summary>Shown only for a service that knows profiles.</summary>
    Profiles,
    Devices,
    Arrangement,
    Files,
    Troubleshooting,
    /// <summary>At the foot of the sidebar, apart from the others.</summary>
    License,
}
