import SwiftUI

/// The five brand colors; everything else uses
/// system semantic colors.
extension Color {
    static let thread = Color("Thread", bundle: .brand)
    static let canvas = Color("Canvas", bundle: .brand)
    static let canvasGrid = Color("Grid", bundle: .brand)
    static let tile = Color("Tile", bundle: .brand)
    static let tileEdge = Color("TileEdge", bundle: .brand)
}

private final class BrandBundle {}

extension Bundle {
    /// The bundle holding the asset catalog: the app, or a test bundle that
    /// compiles the same sources.
    static let brand = Bundle(for: BrandBundle.self)
}

/// How a device's operating system is shown: a type icon plus the OS name,
/// never an OS logo.
struct OSLabel {
    let os: String

    var name: String {
        switch os {
        case "macos": "macOS"
        case "windows": "Windows"
        case "linux": "Linux"
        default: String(localized: "Unknown system")
        }
    }

    /// The device's type is not known, so every device gets a display.
    var symbol: String { "display" }
}
