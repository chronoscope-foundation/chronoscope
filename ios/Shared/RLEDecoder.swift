import Foundation

/// COCO RLE (Run-Length Encoding) decoder for segmentation masks.
///
/// Decodes the compact ASCII string format used by pycocotools.
/// The encoding uses modified LEB128 with a +48 ASCII offset.
///
/// Format:
/// - Each character encodes a 6-bit value (ASCII - 48)
/// - If the high bit (0x20) is set, continue to next character
/// - Run lengths alternate between background (0) and foreground (1)
/// - First run is always background
///
/// Example: "eR\\0X3" decodes to run lengths that fill a binary mask
enum RLEDecoder {
    /// Errors that can occur during RLE decoding.
    enum DecodeError: Error {
        /// The input contains a character outside the valid range (ASCII 48-111).
        case invalidCharacter(Character)
        /// The input ends with an incomplete multi-byte sequence.
        case incompleteSequence
        /// The decoded run lengths don't sum to the expected pixel count (height × width).
        case dimensionMismatch(expected: Int, actual: Int)
    }

    /// Decodes a COCO RLE string into a binary mask.
    ///
    /// - Parameters:
    ///   - counts: The RLE-encoded string (COCO format with +48 ASCII offset)
    ///   - height: Image height in pixels
    ///   - width: Image width in pixels
    /// - Returns: Boolean array of size height × width, row-major order (y, x)
    /// - Throws: `DecodeError` if the input is malformed
    ///
    /// - Note: COCO masks are column-major (Fortran order), so we transpose during decode.
    static func decode(counts: String, height: Int, width: Int) throws -> [Bool] {
        let runLengths = try decodeRuns(counts)
        let totalRun = runLengths.reduce(0, +)
        let expected = height * width
        guard totalRun == expected else {
            throw DecodeError.dimensionMismatch(expected: expected, actual: totalRun)
        }
        return runsToMask(runLengths, height: height, width: width)
    }

    /// Decodes the RLE string into an array of run lengths.
    ///
    /// The COCO format uses modified LEB128:
    /// - Subtract 48 from each ASCII character
    /// - Low 5 bits are the value
    /// - Bit 5 (0x20) is the continuation flag
    /// - Values are accumulated in 5-bit chunks (little-endian)
    ///
    /// Valid ASCII range: 48 ('0') to 111 ('o'), giving 6-bit values 0-63.
    private static func decodeRuns(_ counts: String) throws -> [Int] {
        var runs: [Int] = []
        var currentValue = 0
        var shift = 0

        for char in counts {
            // Valid range is ASCII 48-111 (0x30-0x6F), giving 6-bit values 0-63
            guard let ascii = char.asciiValue,
                  ascii >= 48,
                  ascii <= 111
            else {
                throw DecodeError.invalidCharacter(char)
            }

            // Get the 6-bit value from ASCII (subtract 48)
            let sixBitValue = Int(ascii) - 48

            // Low 5 bits are the actual value
            let valueBits = sixBitValue & 0x1F

            // Accumulate into current value
            currentValue |= valueBits << shift
            shift += 5

            // High bit (0x20) indicates more bytes follow
            let moreBytes = (sixBitValue & 0x20) != 0

            if !moreBytes {
                runs.append(currentValue)
                currentValue = 0
                shift = 0
            }
        }

        // Check for incomplete sequence (ended with continuation flag set)
        if shift > 0 {
            throw DecodeError.incompleteSequence
        }

        return runs
    }

    /// Converts run lengths to a binary mask.
    ///
    /// COCO uses column-major (Fortran) order, so we need to transpose
    /// to get row-major order for display.
    private static func runsToMask(_ runs: [Int], height: Int, width: Int) -> [Bool] {
        let totalPixels = height * width

        // Fill column-major first
        var columnMajor = [Bool](repeating: false, count: totalPixels)
        var position = 0
        var isForeground = false // First run is always background

        for runLength in runs {
            if isForeground {
                for offset in 0 ..< runLength {
                    columnMajor[position + offset] = true
                }
            }
            position += runLength
            isForeground.toggle()
        }

        // Transpose from column-major to row-major
        var mask = [Bool](repeating: false, count: totalPixels)
        for col in 0 ..< width {
            for row in 0 ..< height {
                let colMajorIndex = col * height + row
                let rowMajorIndex = row * width + col
                mask[rowMajorIndex] = columnMajor[colMajorIndex]
            }
        }

        return mask
    }
}

// MARK: - Testing Support

extension RLEDecoder {
    /// Encodes a binary mask into COCO RLE format.
    /// Used primarily for creating test data.
    static func encode(mask: [Bool], height: Int, width: Int) -> String {
        let totalPixels = height * width
        guard mask.count == totalPixels else { return "" }

        // First transpose to column-major order
        var columnMajor = [Bool](repeating: false, count: totalPixels)
        for row in 0 ..< height {
            for col in 0 ..< width {
                let rowMajorIndex = row * width + col
                let colMajorIndex = col * height + row
                columnMajor[colMajorIndex] = mask[rowMajorIndex]
            }
        }

        // Count runs
        var runs: [Int] = []
        var currentRun = 0
        var currentValue = false // Start with background

        for value in columnMajor {
            if value == currentValue {
                currentRun += 1
            } else {
                runs.append(currentRun)
                currentRun = 1
                currentValue = value
            }
        }
        runs.append(currentRun)

        // Encode runs to string
        return encodeRuns(runs)
    }

    /// Encodes run lengths into COCO RLE string format.
    private static func encodeRuns(_ runs: [Int]) -> String {
        var result = ""

        for run in runs {
            var value = run
            repeat {
                // Take low 5 bits
                var chunk = value & 0x1F
                value >>= 5

                // Set continuation bit if more chunks follow
                if value > 0 {
                    chunk |= 0x20
                }

                // Convert to ASCII with +48 offset
                let charValue = chunk + 48
                if let scalar = Unicode.Scalar(charValue) {
                    result.append(Character(scalar))
                }
            } while value > 0
        }

        return result
    }
}
