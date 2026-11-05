using System.Runtime.InteropServices;
using System.Runtime.InteropServices.Marshalling;

namespace InputWeave.Drag;

// OLE drag and drop, for dragging files between computers.
// The interfaces the app implements are
// source-generated COM (trim-safe, nothing depends on the runtime's built-in
// COM interop); the ones it only calls are called through their vtables, as
// in Support/ComCalls.cs. Layouts and slot numbers follow ObjIdl.h, OleIdl.h
// and ShlDisp.h.

[StructLayout(LayoutKind.Sequential)]
internal struct PointL
{
    public int X;
    public int Y;
}

[StructLayout(LayoutKind.Sequential)]
internal struct FormatEtc
{
    public ushort Format;
    public nint Device;
    public uint Aspect;
    public int Index;
    public uint Medium;
}

[StructLayout(LayoutKind.Sequential)]
internal struct StgMedium
{
    public uint Medium;
    public nint Handle;
    public nint ReleaseOwner;
}

[GeneratedComInterface]
[Guid("00000122-0000-0000-C000-000000000046")]
internal unsafe partial interface IDropTarget
{
    [PreserveSig]
    int DragEnter(nint data, uint keys, PointL point, uint* effect);

    [PreserveSig]
    int DragOver(uint keys, PointL point, uint* effect);

    [PreserveSig]
    int DragLeave();

    [PreserveSig]
    int Drop(nint data, uint keys, PointL point, uint* effect);
}

[GeneratedComInterface]
[Guid("00000121-0000-0000-C000-000000000046")]
internal partial interface IDropSource
{
    [PreserveSig]
    int QueryContinueDrag(int escapePressed, uint keys);

    [PreserveSig]
    int GiveFeedback(uint effect);
}

[GeneratedComInterface]
[Guid("0000010e-0000-0000-C000-000000000046")]
internal unsafe partial interface IDataObject
{
    [PreserveSig]
    int GetData(FormatEtc* format, StgMedium* medium);

    [PreserveSig]
    int GetDataHere(FormatEtc* format, StgMedium* medium);

    [PreserveSig]
    int QueryGetData(FormatEtc* format);

    [PreserveSig]
    int GetCanonicalFormatEtc(FormatEtc* format, FormatEtc* result);

    [PreserveSig]
    int SetData(FormatEtc* format, StgMedium* medium, int release);

    [PreserveSig]
    int EnumFormatEtc(uint direction, nint* enumerator);

    [PreserveSig]
    int DAdvise(FormatEtc* format, uint flags, nint sink, uint* connection);

    [PreserveSig]
    int DUnadvise(uint connection);

    [PreserveSig]
    int EnumDAdvise(nint* enumerator);
}

/// <summary>
/// Lets a drop target take the files on its own time (IAsyncOperation):
/// File Explorer then copies on a thread of its own and shows its progress
/// dialog, instead of holding up its window until every file has arrived.
/// </summary>
[GeneratedComInterface]
[Guid("3D8B0590-F691-11d2-8EA9-006097DF5BD4")]
internal unsafe partial interface IDataObjectAsyncCapability
{
    [PreserveSig]
    int SetAsyncMode(int doOpAsync);

    [PreserveSig]
    int GetAsyncMode(int* isOpAsync);

    [PreserveSig]
    int StartOperation(nint bindContext);

    [PreserveSig]
    int InOperation(int* inAsyncOp);

    [PreserveSig]
    int EndOperation(int result, nint bindContext, uint effects);
}

internal static unsafe partial class Ole
{
    public const int S_OK = 0;
    public const int S_FALSE = 1;
    public const int E_NOTIMPL = unchecked((int)0x80004001);
    public const int E_FAIL = unchecked((int)0x80004005);
    public const int E_UNEXPECTED = unchecked((int)0x8000FFFF);
    public const int E_INVALIDARG = unchecked((int)0x80070057);
    public const int DV_E_FORMATETC = unchecked((int)0x80040064);
    public const int DV_E_TYMED = unchecked((int)0x80040069);
    public const int DV_E_LINDEX = unchecked((int)0x80040068);
    public const int OLE_E_ADVISENOTSUPPORTED = unchecked((int)0x80040003);
    public const int DATA_S_SAMEFORMATETC = 0x00040130;
    public const int DRAGDROP_S_DROP = 0x00040100;
    public const int DRAGDROP_S_CANCEL = 0x00040101;
    public const int DRAGDROP_S_USEDEFAULTCURSORS = 0x00040102;

    public const uint DROPEFFECT_NONE = 0;
    public const uint DROPEFFECT_COPY = 1;

    public const uint MK_LBUTTON = 0x1;
    public const uint MK_RBUTTON = 0x2;
    public const uint MK_MBUTTON = 0x10;

    public const ushort CF_HDROP = 15;
    public const uint DVASPECT_CONTENT = 1;
    public const uint TYMED_HGLOBAL = 1;
    public const uint TYMED_ISTREAM = 4;
    public const uint DATADIR_GET = 1;

    [LibraryImport("ole32.dll")]
    public static partial int OleInitialize(nint reserved);

    [LibraryImport("ole32.dll")]
    public static partial void OleUninitialize();

    [LibraryImport("ole32.dll")]
    public static partial int RegisterDragDrop(nint hwnd, nint target);

    [LibraryImport("ole32.dll")]
    public static partial int RevokeDragDrop(nint hwnd);

    [LibraryImport("ole32.dll")]
    public static partial int DoDragDrop(nint data, nint source, uint okEffects, out uint effect);

    [LibraryImport("ole32.dll")]
    public static partial void ReleaseStgMedium(StgMedium* medium);

    /// <summary>Puts a data object on the clipboard, or, with 0, empties it if this thread's object is there.</summary>
    [LibraryImport("ole32.dll")]
    public static partial int OleSetClipboard(nint data);

    /// <summary>S_OK while <paramref name="data"/> is the clipboard's.</summary>
    [LibraryImport("ole32.dll")]
    public static partial int OleIsCurrentClipboard(nint data);

    public const uint COINIT_MULTITHREADED = 0;

    [LibraryImport("ole32.dll")]
    public static partial int CoInitializeEx(nint reserved, uint coinit);

    [LibraryImport("ole32.dll")]
    public static partial int CoMarshalInterThreadInterfaceInStream(in Guid iid, nint unknown, out nint stream);

    [LibraryImport("ole32.dll")]
    public static partial int CoGetInterfaceAndReleaseStream(nint stream, in Guid iid, out nint pointer);

    public const ushort CF_UNICODETEXT = 13;

    [LibraryImport("shell32.dll", EntryPoint = "DragQueryFileW")]
    public static partial uint DragQueryFile(nint drop, uint index, char* file, uint size);

    [LibraryImport("shell32.dll")]
    public static partial int SHCreateStdEnumFmtEtc(uint count, FormatEtc* formats, out nint enumerator);

    [LibraryImport("shlwapi.dll", StringMarshalling = StringMarshalling.Utf16)]
    public static partial int SHCreateStreamOnFileEx(string file, uint mode, uint attributes, int create, nint template, out nint stream);

    public const uint STGM_READ = 0x0;
    public const uint STGM_SHARE_DENY_WRITE = 0x20;

    [LibraryImport("user32.dll", EntryPoint = "RegisterClipboardFormatW", StringMarshalling = StringMarshalling.Utf16)]
    public static partial uint RegisterClipboardFormat(string name);

    [LibraryImport("kernel32.dll")]
    public static partial nint GlobalAlloc(uint flags, nuint bytes);

    [LibraryImport("kernel32.dll")]
    public static partial nint GlobalLock(nint memory);

    [LibraryImport("kernel32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool GlobalUnlock(nint memory);

    [LibraryImport("kernel32.dll")]
    public static partial nuint GlobalSize(nint memory);

    [LibraryImport("kernel32.dll")]
    public static partial nint GlobalFree(nint memory);

    private const uint GMEM_MOVEABLE = 0x2;

    /// <summary>A movable global memory block with these bytes, for an HGLOBAL medium; 0 when out of memory.</summary>
    public static nint ToGlobal(ReadOnlySpan<byte> bytes)
    {
        var memory = GlobalAlloc(GMEM_MOVEABLE, (nuint)Math.Max(1, bytes.Length));
        if (memory == 0)
        {
            return 0;
        }
        var at = GlobalLock(memory);
        if (at == 0)
        {
            GlobalFree(memory);
            return 0;
        }
        bytes.CopyTo(new Span<byte>((void*)at, bytes.Length));
        GlobalUnlock(memory);
        return memory;
    }

    /// <summary>The bytes of a global memory block.</summary>
    public static byte[] FromGlobal(nint memory)
    {
        var size = (int)Math.Min(GlobalSize(memory), int.MaxValue);
        var at = GlobalLock(memory);
        if (at == 0)
        {
            return [];
        }
        try
        {
            return new ReadOnlySpan<byte>((void*)at, size).ToArray();
        }
        finally
        {
            GlobalUnlock(memory);
        }
    }

    private static nint* Slots(nint instance) => *(nint**)instance;

    /// <summary>IDataObject::GetData on an object another process passed in.</summary>
    public static int GetData(nint data, FormatEtc* format, StgMedium* medium) =>
        ((delegate* unmanaged[Stdcall]<nint, FormatEtc*, StgMedium*, int>)Slots(data)[3])(data, format, medium);

    /// <summary>
    /// The files and folders a drag carries as CF_HDROP, which is what File
    /// Explorer and most apps put on a drag of files; null without it.
    /// </summary>
    public static IReadOnlyList<string>? DroppedPaths(nint data)
    {
        if (data == 0)
        {
            return null;
        }
        var format = new FormatEtc { Format = CF_HDROP, Aspect = DVASPECT_CONTENT, Index = -1, Medium = TYMED_HGLOBAL };
        var medium = new StgMedium();
        if (GetData(data, &format, &medium) != S_OK || medium.Medium != TYMED_HGLOBAL || medium.Handle == 0)
        {
            return null;
        }
        try
        {
            uint count = DragQueryFile(medium.Handle, uint.MaxValue, null, 0);
            var paths = new List<string>((int)Math.Min(count, 10_000));
            for (uint i = 0; i < count; i++)
            {
                uint length = DragQueryFile(medium.Handle, i, null, 0);
                if (length == 0)
                {
                    continue;
                }
                var buffer = new char[length + 1];
                fixed (char* at = buffer)
                {
                    uint copied = DragQueryFile(medium.Handle, i, at, length + 1);
                    paths.Add(new string(at, 0, (int)copied));
                }
            }
            return paths;
        }
        finally
        {
            ReleaseStgMedium(&medium);
        }
    }

    /// <summary>The source-generated COM wrappers for the objects the app hands to OLE.</summary>
    public static readonly StrategyBasedComWrappers Wrappers = new();

    /// <summary>
    /// A COM pointer to <paramref name="instance"/> as <typeparamref name="T"/>,
    /// with a reference the caller releases.
    /// </summary>
    public static nint ComPointer<T>(object instance)
    {
        var unknown = Wrappers.GetOrCreateComInterfaceForObject(instance, CreateComInterfaceFlags.None);
        try
        {
            var iid = typeof(T).GUID;
            Marshal.ThrowExceptionForHR(Marshal.QueryInterface(unknown, in iid, out var pointer));
            return pointer;
        }
        finally
        {
            Marshal.Release(unknown);
        }
    }
}

/// <summary>The little of Win32 windowing the drag windows need.</summary>
internal static unsafe partial class Win32
{
    [StructLayout(LayoutKind.Sequential)]
    public struct WindowClass
    {
        public uint Size;
        public uint Style;
        public delegate* unmanaged[Stdcall]<nint, uint, nint, nint, nint> Procedure;
        public int ClassExtra;
        public int WindowExtra;
        public nint Instance;
        public nint Icon;
        public nint Cursor;
        public nint Background;
        public char* MenuName;
        public char* ClassName;
        public nint SmallIcon;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Message
    {
        public nint Window;
        public uint Id;
        public nint WParam;
        public nint LParam;
        public uint Time;
        public int X;
        public int Y;
        public uint Private;
    }

    [StructLayout(LayoutKind.Sequential)]
    public struct Point
    {
        public int X;
        public int Y;
    }

    [LibraryImport("user32.dll", EntryPoint = "RegisterClassExW")]
    public static partial ushort RegisterClass(WindowClass* windowClass);

    [LibraryImport("user32.dll", EntryPoint = "CreateWindowExW", StringMarshalling = StringMarshalling.Utf16)]
    public static partial nint CreateWindow(
        uint exStyle, string className, string title, uint style, int x, int y, int width, int height,
        nint parent, nint menu, nint instance, nint parameter);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool DestroyWindow(nint hwnd);

    [LibraryImport("user32.dll", EntryPoint = "DefWindowProcW")]
    public static partial nint DefWindowProc(nint hwnd, uint message, nint wParam, nint lParam);

    [LibraryImport("user32.dll", EntryPoint = "GetMessageW")]
    public static partial int GetMessage(Message* message, nint hwnd, uint first, uint last);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool TranslateMessage(Message* message);

    [LibraryImport("user32.dll", EntryPoint = "DispatchMessageW")]
    public static partial nint DispatchMessage(Message* message);

    [LibraryImport("user32.dll", EntryPoint = "PostMessageW")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool PostMessage(nint hwnd, uint message, nint wParam, nint lParam);

    [LibraryImport("user32.dll")]
    public static partial void PostQuitMessage(int exitCode);

    [LibraryImport("user32.dll")]
    public static partial nuint SetTimer(nint hwnd, nuint id, uint milliseconds, nint callback);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool KillTimer(nint hwnd, nuint id);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool SetWindowPos(nint hwnd, nint after, int x, int y, int cx, int cy, uint flags);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool ShowWindow(nint hwnd, int command);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool SetLayeredWindowAttributes(nint hwnd, uint key, byte alpha, uint flags);

    [LibraryImport("user32.dll", EntryPoint = "GetWindowLongPtrW")]
    public static partial nint GetWindowLongPtr(nint hwnd, int index);

    [LibraryImport("user32.dll", EntryPoint = "SetWindowLongPtrW")]
    public static partial nint SetWindowLongPtr(nint hwnd, int index, nint value);

    [LibraryImport("user32.dll")]
    public static partial short GetAsyncKeyState(int key);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool GetCursorPos(out Point point);

    [LibraryImport("kernel32.dll", EntryPoint = "GetModuleHandleW")]
    public static partial nint GetModuleHandle(nint name);

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    public static partial bool ReleaseCapture();

    [StructLayout(LayoutKind.Sequential)]
    private struct MouseInput
    {
        public int Dx;
        public int Dy;
        public uint Data;
        public uint Flags;
        public uint Time;
        public nint ExtraInfo;
    }

    /// <summary>INPUT, as on 64-bit Windows, the only kind the app is built for.</summary>
    [StructLayout(LayoutKind.Explicit, Size = 40)]
    private struct Input
    {
        [FieldOffset(0)]
        public uint Type;

        [FieldOffset(8)]
        public MouseInput Mouse;
    }

    [LibraryImport("user32.dll", SetLastError = true)]
    private static partial uint SendInput(uint count, Input* inputs, int size);

    private const uint MOUSEEVENTF_LEFTDOWN = 0x2;
    private const uint MOUSEEVENTF_LEFTUP = 0x4;
    private const uint MOUSEEVENTF_RIGHTDOWN = 0x8;
    private const uint MOUSEEVENTF_RIGHTUP = 0x10;
    private const uint MOUSEEVENTF_MIDDLEDOWN = 0x20;
    private const uint MOUSEEVENTF_MIDDLEUP = 0x40;

    /// <summary>
    /// Lets go of the mouse buttons held now and presses them again, where
    /// the cursor is, in one go: the press then lands on whatever window is
    /// there now. False when Windows did not take the input (as when the
    /// window in front runs as administrator).
    /// </summary>
    public static bool PressAgain()
    {
        var held = new List<(uint Up, uint Down)>();
        if ((GetAsyncKeyState(VK_LBUTTON) & 0x8000) != 0)
        {
            held.Add((MOUSEEVENTF_LEFTUP, MOUSEEVENTF_LEFTDOWN));
        }
        if ((GetAsyncKeyState(VK_RBUTTON) & 0x8000) != 0)
        {
            held.Add((MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_RIGHTDOWN));
        }
        if ((GetAsyncKeyState(VK_MBUTTON) & 0x8000) != 0)
        {
            held.Add((MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MIDDLEDOWN));
        }
        if (held.Count == 0)
        {
            return false;
        }
        var inputs = stackalloc Input[held.Count * 2];
        for (int i = 0; i < held.Count; i++)
        {
            inputs[i] = new Input { Mouse = new MouseInput { Flags = held[i].Up } };
            inputs[held.Count + i] = new Input { Mouse = new MouseInput { Flags = held[i].Down } };
        }
        return SendInput((uint)(held.Count * 2), inputs, sizeof(Input)) == held.Count * 2;
    }

    [LibraryImport("gdi32.dll")]
    public static partial nint GetStockObject(int index);

    [LibraryImport("user32.dll", EntryPoint = "LoadCursorW")]
    public static partial nint LoadCursor(nint instance, nint name);

    public const int BLACK_BRUSH = 4;
    public static readonly nint IDC_ARROW = 32512;
    public const int GWL_EXSTYLE = -20;
    public const uint WS_POPUP = 0x80000000;
    public const uint WS_EX_TOPMOST = 0x8;
    public const uint WS_EX_TRANSPARENT = 0x20;
    public const uint WS_EX_TOOLWINDOW = 0x80;
    public const uint WS_EX_LAYERED = 0x80000;
    public const uint WS_EX_NOACTIVATE = 0x08000000;
    public const uint LWA_ALPHA = 0x2;
    public static readonly nint HWND_TOPMOST = -1;
    public static readonly nint HWND_MESSAGE = -3;
    public const uint SWP_NOSIZE = 0x1;
    public const uint SWP_NOMOVE = 0x2;
    public const uint SWP_NOACTIVATE = 0x10;
    public const uint SWP_SHOWWINDOW = 0x40;
    public const int SW_HIDE = 0;
    public const int SW_SHOWNOACTIVATE = 4;
    public const uint WM_DESTROY = 0x0002;
    public const uint WM_TIMER = 0x0113;
    public const uint WM_MOUSEACTIVATE = 0x0021;
    public const uint WM_LBUTTONDOWN = 0x0201;
    public const uint WM_RBUTTONDOWN = 0x0204;
    public const uint WM_MBUTTONDOWN = 0x0207;
    public const uint WM_APP = 0x8000;
    public const int MA_NOACTIVATE = 3;
    public const int VK_LBUTTON = 0x01;
    public const int VK_RBUTTON = 0x02;
    public const int VK_MBUTTON = 0x04;
    public const int VK_ESCAPE = 0x1B;

    /// <summary>Whether a mouse button is down now, whichever process has the input.</summary>
    public static bool AnyButtonDown() =>
        (GetAsyncKeyState(VK_LBUTTON) & 0x8000) != 0
        || (GetAsyncKeyState(VK_RBUTTON) & 0x8000) != 0
        || (GetAsyncKeyState(VK_MBUTTON) & 0x8000) != 0;

    /// <summary>Whether Escape is down now, whichever window has the keyboard.</summary>
    public static bool EscapeDown() => (GetAsyncKeyState(VK_ESCAPE) & 0x8000) != 0;

    public static (int X, int Y) CursorPosition() => GetCursorPos(out var p) ? (p.X, p.Y) : (0, 0);

    private static readonly Dictionary<string, ushort> Registered = [];

    /// <summary>Registers a window class whose messages go to <paramref name="procedure"/>, once per name.</summary>
    public static void EnsureClass(string name, delegate* unmanaged[Stdcall]<nint, uint, nint, nint, nint> procedure)
    {
        lock (Registered)
        {
            if (Registered.ContainsKey(name))
            {
                return;
            }
            fixed (char* className = name)
            {
                var windowClass = new WindowClass
                {
                    Size = (uint)sizeof(WindowClass),
                    Procedure = procedure,
                    Instance = GetModuleHandle(0),
                    Cursor = LoadCursor(0, IDC_ARROW),
                    Background = GetStockObject(BLACK_BRUSH),
                    ClassName = className,
                };
                Registered[name] = RegisterClass(&windowClass);
            }
        }
    }
}
