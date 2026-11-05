namespace InputWeave.Drag;

/// <summary>
/// What files that do not exist yet look like to whoever takes them, a
/// drop target or File Explorer pasting from the clipboard: a descriptor of
/// every file and folder (CFSTR_FILEDESCRIPTORW), each file's contents read
/// one by one (CFSTR_FILECONTENTS), and copy as the effect wanted. Shared by
/// a drag offered here (<see cref="FilePromise"/>) and files on the
/// clipboard (<see cref="ClipboardFilePromise"/>).
/// </summary>
internal static unsafe class PromisedFormats
{
    public static readonly ushort Descriptor = (ushort)Ole.RegisterClipboardFormat("FileGroupDescriptorW");
    public static readonly ushort Contents = (ushort)Ole.RegisterClipboardFormat("FileContents");
    public static readonly ushort PreferredEffect = (ushort)Ole.RegisterClipboardFormat("Preferred DropEffect");

    /// <summary>Hands <paramref name="bytes"/> over in an HGLOBAL medium.</summary>
    public static int Global(byte[] bytes, StgMedium* medium)
    {
        var memory = Ole.ToGlobal(bytes);
        if (memory == 0)
        {
            return Ole.E_FAIL;
        }
        *medium = new StgMedium { Medium = Ole.TYMED_HGLOBAL, Handle = memory };
        return Ole.S_OK;
    }

    /// <summary>Hands over a stream on the file at <paramref name="source"/>, which is read and not changed.</summary>
    public static int Stream(string source, StgMedium* medium)
    {
        int opened = Ole.SHCreateStreamOnFileEx(source, Ole.STGM_READ | Ole.STGM_SHARE_DENY_WRITE, 0, 0, 0, out var stream);
        if (opened != Ole.S_OK)
        {
            return opened;
        }
        *medium = new StgMedium { Medium = Ole.TYMED_ISTREAM, Handle = stream };
        return Ole.S_OK;
    }

    /// <summary>The descriptor, the contents and the effect, for an enumerator of what a data object holds.</summary>
    public static IEnumerable<FormatEtc> Files() =>
    [
        new FormatEtc { Format = Descriptor, Aspect = Ole.DVASPECT_CONTENT, Index = -1, Medium = Ole.TYMED_HGLOBAL },
        new FormatEtc { Format = Contents, Aspect = Ole.DVASPECT_CONTENT, Index = -1, Medium = Ole.TYMED_ISTREAM },
        new FormatEtc { Format = PreferredEffect, Aspect = Ole.DVASPECT_CONTENT, Index = -1, Medium = Ole.TYMED_HGLOBAL },
    ];

    /// <summary>An enumerator of <paramref name="formats"/>, as IDataObject::EnumFormatEtc hands over.</summary>
    public static int Enumerate(IReadOnlyList<FormatEtc> formats, nint* enumerator)
    {
        var array = formats.ToArray();
        fixed (FormatEtc* first = array)
        {
            int result = Ole.SHCreateStdEnumFmtEtc((uint)array.Length, first, out var created);
            *enumerator = created;
            return result;
        }
    }
}
