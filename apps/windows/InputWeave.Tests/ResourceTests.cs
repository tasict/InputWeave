using System.Text.RegularExpressions;
using System.Xml.Linq;
using InputWeave.Panel;
using Xunit;

namespace InputWeave.Tests;

/// <summary>The app's strings: English is the source, and zh-TW has every one of them.</summary>
public partial class ResourceTests
{
    private static Dictionary<string, string> Load(string language)
    {
        var file = Path.Combine(AppContext.BaseDirectory, "Strings", language, "Resources.resw");
        return XDocument.Load(file).Root!.Elements("data")
            .ToDictionary(d => (string)d.Attribute("name")!, d => (string)d.Element("value")!);
    }

    [GeneratedRegex(@"\{\d+\}")]
    private static partial Regex Placeholder();

    private static string[] Placeholders(string text) =>
        Placeholder().Matches(text).Select(m => m.Value).Order().ToArray();

    [Fact]
    public void BothLanguagesHaveTheSameStrings()
    {
        var en = Load("en-US");
        var zh = Load("zh-TW");
        Assert.Equal(en.Keys.Order(), zh.Keys.Order());
        foreach (var (key, text) in en)
        {
            Assert.False(string.IsNullOrWhiteSpace(zh[key]), key);
            Assert.True(Placeholders(text).SequenceEqual(Placeholders(zh[key])), $"{key}: placeholders differ");
        }
    }

    [Fact]
    public void EveryKeyTheCodeUsesExists()
    {
        var en = Load("en-US");
        var keys = typeof(TextKeys).GetFields()
            .Where(f => f.IsLiteral && f.FieldType == typeof(string))
            .Select(f => (string)f.GetRawConstantValue()!)
            .ToList();
        Assert.NotEmpty(keys);
        Assert.All(keys, key => Assert.True(en.ContainsKey(key), $"missing {key}"));
    }

    [Fact]
    public void ChineseUsesTaiwaneseTerms()
    {
        // Terms of mainland China that Taiwan's Chinese avoids.
        string[] avoided = ["屏幕", "文件夾", "設置", "鼠標", "網絡", "默認", "剪貼板", "剪切板", "鏈接", "節點", "用戶端", "快捷鍵", "熱鍵", "輔助鍵", "錄入", "識別顯示器", "辨識顯示器"];
        foreach (var (_, text) in Load("zh-TW"))
        {
            Assert.All(avoided, word => Assert.DoesNotContain(word, text));
        }
    }
}
