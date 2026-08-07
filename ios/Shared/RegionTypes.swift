import Accelerate
import CoreGraphics
import Foundation

// MARK: - RGB Color

/// A simple RGB color representation using UInt8 components.
/// Avoids SwiftUI dependency and unnecessary conversions.
struct RGBColor: Equatable, Sendable {
    let red: UInt8
    let green: UInt8
    let blue: UInt8

    init(_ red: UInt8, _ green: UInt8, _ blue: UInt8) {
        self.red = red
        self.green = green
        self.blue = blue
    }
}

// MARK: - Region Colors

/// Kelly's 22 colors of maximum contrast (1965), minus white and black = 20 colors.
/// Reference: https://gist.github.com/ollieglass/f6ddd781eeae1d24e391265432297538
enum RegionColors {
    static let palette: [RGBColor] = [
        RGBColor(243, 195, 0), // Yellow
        RGBColor(135, 86, 146), // Purple
        RGBColor(243, 132, 0), // Orange
        RGBColor(161, 202, 241), // Light blue
        RGBColor(190, 0, 50), // Red
        RGBColor(194, 178, 128), // Buff
        RGBColor(132, 132, 130), // Gray
        RGBColor(0, 136, 86), // Green
        RGBColor(230, 143, 172), // Pink
        RGBColor(0, 103, 165), // Blue
        RGBColor(249, 147, 121), // Apricot
        RGBColor(96, 78, 151), // Violet
        RGBColor(246, 166, 0), // Orange yellow
        RGBColor(179, 68, 108), // Purplish red
        RGBColor(220, 211, 0), // Greenish yellow
        RGBColor(136, 45, 23), // Brown
        RGBColor(141, 182, 0), // Yellow green
        RGBColor(101, 69, 34), // Brownish orange
        RGBColor(226, 88, 34), // Reddish orange
        RGBColor(43, 61, 38) // Olive green
    ]

    /// Returns the color for a given region index (0-indexed).
    static func color(forIndex index: Int) -> RGBColor {
        palette[index % palette.count]
    }

    /// Opacity for unselected region overlays.
    static let overlayOpacity: Double = 0.25

    /// Opacity for the selected region overlay.
    static let selectedOpacity: Double = 0.50

    /// Opacity for dimmed (non-selected) regions when one is selected.
    static let dimmedOpacity: Double = 0.15
}

// MARK: - Decoded Region

/// A region with its decoded binary mask ready for rendering.
/// Generic over the ID type to support both Int and String region identifiers.
struct DecodedRegion<ID: Hashable & Sendable>: Identifiable, @unchecked Sendable {
    let id: ID
    let mask: [Bool]
    let width: Int
    let height: Int

    /// Pre-rendered mask image for efficient Canvas drawing.
    /// Computed once at initialization to avoid recreation on every render.
    let cachedMaskImage: CGImage?

    /// Pre-computed edge mask for outline rendering.
    /// Edge pixels are where original mask differs from eroded mask.
    private let edgeMask: [Bool]

    /// The RGB color for this region based on its index in the palette.
    let rgb: RGBColor

    init(id: ID, mask: [Bool], width: Int, height: Int, colorIndex: Int? = nil) {
        self.id = id
        self.mask = mask
        self.width = width
        self.height = height

        // Use provided colorIndex, or hash-based index for consistent coloring
        let index = colorIndex ?? abs(id.hashValue) % RegionColors.palette.count
        self.rgb = RegionColors.color(forIndex: index)

        // Pre-compute edge mask using vImage erosion
        self.edgeMask = Self.computeEdgeMask(mask: mask, width: width, height: height)

        self.cachedMaskImage = Self.createMaskImage(
            mask: mask,
            width: width,
            height: height,
            rgb: self.rgb
        )
    }

    /// Computes edge pixels using vImage erosion: edge = original XOR eroded.
    private static func computeEdgeMask(mask: [Bool], width: Int, height: Int) -> [Bool] {
        let totalPixels = width * height
        guard totalPixels > 0 else { return [] }

        // Convert Bool mask to UInt8 buffer (255 = foreground, 0 = background)
        var original = mask.map { $0 ? UInt8(255) : UInt8(0) }
        var eroded = [UInt8](repeating: 0, count: totalPixels)

        // 3x3 structuring element (all 1s) for 8-neighbor erosion
        let kernel: [UInt8] = [
            255, 255, 255,
            255, 255, 255,
            255, 255, 255
        ]

        original.withUnsafeMutableBufferPointer { srcPtr in
            eroded.withUnsafeMutableBufferPointer { dstPtr in
                var src = vImage_Buffer(
                    data: srcPtr.baseAddress,
                    height: vImagePixelCount(height),
                    width: vImagePixelCount(width),
                    rowBytes: width
                )
                var dst = vImage_Buffer(
                    data: dstPtr.baseAddress,
                    height: vImagePixelCount(height),
                    width: vImagePixelCount(width),
                    rowBytes: width
                )

                // Erode: minimum filter with 3x3 kernel
                vImageErode_Planar8(&src, &dst, 0, 0, kernel, 3, 3, vImage_Flags(kvImageNoFlags))
            }
        }

        // Edge = original XOR eroded (pixels that are in original but not in eroded)
        return zip(original, eroded).map { orig, erod in
            orig != 0 && erod == 0
        }
    }

    /// Creates a CGImage of the mask filled with the given color at full opacity.
    /// The image is at 1:1 scale with the original image dimensions.
    private static func createMaskImage(
        mask: [Bool],
        width: Int,
        height: Int,
        rgb: RGBColor
    ) -> CGImage? {
        // Create RGBA pixel buffer
        var pixels = [UInt8](repeating: 0, count: width * height * 4)

        for row in 0 ..< height {
            for col in 0 ..< width {
                let maskIndex = row * width + col
                let pixelIndex = maskIndex * 4

                if maskIndex < mask.count, mask[maskIndex] {
                    pixels[pixelIndex] = rgb.red
                    pixels[pixelIndex + 1] = rgb.green
                    pixels[pixelIndex + 2] = rgb.blue
                    pixels[pixelIndex + 3] = 255 // Full alpha - opacity applied when drawing
                }
                // Else: pixels remain 0 (transparent)
            }
        }

        // Create CGImage from pixel buffer
        guard let context = CGContext(
            data: &pixels,
            width: width,
            height: height,
            bitsPerComponent: 8,
            bytesPerRow: width * 4,
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
        ) else { return nil }

        return context.makeImage()
    }

    /// Creates a CGPath outlining the mask boundary for stroke rendering.
    /// Uses pre-computed edge mask from vImage erosion.
    /// Merges adjacent horizontal edge pixels into single rects to reduce path complexity.
    func createOutlinePath(scaleX: CGFloat, scaleY: CGFloat) -> CGPath {
        let path = CGMutablePath()

        for row in 0 ..< height {
            var col = 0
            while col < width {
                let index = row * width + col
                guard index < edgeMask.count, edgeMask[index] else {
                    col += 1
                    continue
                }

                // Start of a horizontal run of edge pixels
                let startCol = col
                col += 1
                while col < width {
                    let nextIndex = row * width + col
                    guard nextIndex < edgeMask.count, edgeMask[nextIndex] else { break }
                    col += 1
                }

                path.addRect(CGRect(
                    x: CGFloat(startCol) * scaleX,
                    y: CGFloat(row) * scaleY,
                    width: CGFloat(col - startCol) * scaleX,
                    height: scaleY
                ))
            }
        }

        return path
    }
}

// Convenience initializer for Int IDs that uses the ID value for color indexing
extension DecodedRegion where ID == Int {
    init(id: Int, mask: [Bool], width: Int, height: Int) {
        // Use (id - 1) for 1-indexed IDs to get consistent colors starting from palette[0]
        self.init(id: id, mask: mask, width: width, height: height, colorIndex: id - 1)
    }
}
