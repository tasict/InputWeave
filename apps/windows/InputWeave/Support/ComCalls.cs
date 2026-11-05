using System.Runtime.InteropServices;
using InputWeave.Setup;

namespace InputWeave.Support;

/// <summary>
/// The two COM objects the app uses, called through their vtables so
/// nothing depends on the runtime's built-in COM interop: the Network List
/// Manager, for whether the network is set to Public, and the Shell's
/// shortcut object, for the Send To shortcut. The slot numbers follow
/// netlistmgr.h and ShObjIdl_core.h; IDispatch adds four slots after
/// IUnknown's three.
/// </summary>
internal static unsafe partial class ComCalls
{
    private const uint CLSCTX_ALL = 0x17;
    private const uint CLSCTX_INPROC_SERVER = 0x1;

    private static readonly Guid NetworkListManagerClass = new("DCB00C01-570F-4A9B-8D69-199FDBA5723B");
    private static readonly Guid INetworkListManager = new("DCB00000-570F-4A9B-8D69-199FDBA5723B");
    private const int NLM_ENUM_NETWORK_CONNECTED = 0x1;

    private static readonly Guid ShellLinkClass = new("00021401-0000-0000-C000-000000000046");
    private static readonly Guid IShellLinkW = new("000214F9-0000-0000-C000-000000000046");
    private static readonly Guid IPersistFile = new("0000010b-0000-0000-C000-000000000046");

    [LibraryImport("ole32.dll")]
    private static partial int CoCreateInstance(in Guid clsid, nint outer, uint context, in Guid iid, out nint instance);

    private static nint* Slots(nint instance) => *(nint**)instance;

    /// <summary>
    /// The categories of the networks this computer is connected to now:
    /// Public, Private or domain. Empty when Windows cannot say.
    /// </summary>
    public static IReadOnlyList<NetworkCategory> ConnectedNetworkCategories()
    {
        var categories = new List<NetworkCategory>();
        nint manager = 0;
        nint networks = 0;
        try
        {
            Marshal.ThrowExceptionForHR(CoCreateInstance(NetworkListManagerClass, 0, CLSCTX_ALL, INetworkListManager, out manager));
            // INetworkListManager::GetNetworks
            var getNetworks = (delegate* unmanaged[Stdcall]<nint, int, nint*, int>)Slots(manager)[7];
            Marshal.ThrowExceptionForHR(getNetworks(manager, NLM_ENUM_NETWORK_CONNECTED, &networks));
            // IEnumNetworks::Next
            var next = (delegate* unmanaged[Stdcall]<nint, uint, nint*, uint*, int>)Slots(networks)[8];
            while (true)
            {
                nint network = 0;
                uint fetched = 0;
                if (next(networks, 1, &network, &fetched) != 0 || fetched == 0 || network == 0)
                {
                    break;
                }
                try
                {
                    // INetwork::GetCategory
                    var getCategory = (delegate* unmanaged[Stdcall]<nint, int*, int>)Slots(network)[18];
                    int category;
                    if (getCategory(network, &category) == 0)
                    {
                        categories.Add((NetworkCategory)category);
                    }
                }
                finally
                {
                    Marshal.Release(network);
                }
            }
        }
        catch (Exception e) when (e is COMException or InvalidCastException or UnauthorizedAccessException)
        {
            AppLog.Warn($"cannot read the network category: {e.Message}");
        }
        finally
        {
            if (networks != 0)
            {
                Marshal.Release(networks);
            }
            if (manager != 0)
            {
                Marshal.Release(manager);
            }
        }
        return categories;
    }

    /// <summary>Writes a shortcut to <paramref name="target"/> at <paramref name="file"/>, a .lnk path.</summary>
    public static void SaveShortcut(string file, string target, string arguments, string description)
    {
        nint link = 0;
        nint persist = 0;
        try
        {
            Marshal.ThrowExceptionForHR(CoCreateInstance(ShellLinkClass, 0, CLSCTX_INPROC_SERVER, IShellLinkW, out link));
            var slots = Slots(link);
            fixed (char* path = target)
            fixed (char* args = arguments)
            fixed (char* about = description)
            fixed (char* folder = Path.GetDirectoryName(target) ?? "")
            {
                // SetPath, SetArguments, SetDescription, SetWorkingDirectory, SetIconLocation
                Marshal.ThrowExceptionForHR(((delegate* unmanaged[Stdcall]<nint, char*, int>)slots[20])(link, path));
                Marshal.ThrowExceptionForHR(((delegate* unmanaged[Stdcall]<nint, char*, int>)slots[11])(link, args));
                Marshal.ThrowExceptionForHR(((delegate* unmanaged[Stdcall]<nint, char*, int>)slots[7])(link, about));
                Marshal.ThrowExceptionForHR(((delegate* unmanaged[Stdcall]<nint, char*, int>)slots[9])(link, folder));
                Marshal.ThrowExceptionForHR(((delegate* unmanaged[Stdcall]<nint, char*, int, int>)slots[17])(link, path, 0));
            }
            Marshal.ThrowExceptionForHR(Marshal.QueryInterface(link, in IPersistFile, out persist));
            fixed (char* name = file)
            {
                // IPersistFile::Save
                Marshal.ThrowExceptionForHR(((delegate* unmanaged[Stdcall]<nint, char*, int, int>)Slots(persist)[6])(persist, name, 1));
            }
        }
        finally
        {
            if (persist != 0)
            {
                Marshal.Release(persist);
            }
            if (link != 0)
            {
                Marshal.Release(link);
            }
        }
    }
}
