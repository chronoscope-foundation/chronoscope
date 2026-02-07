@testable import Chronoscope
import XCTest

final class RLEDecoderTests: XCTestCase {
    // MARK: - Round-Trip Tests

    func testRoundTrip_singlePixelForeground() throws {
        // 1x1 image, fully foreground
        let mask = [true]
        let encoded = RLEDecoder.encode(mask: mask, height: 1, width: 1)
        let decoded = try RLEDecoder.decode(counts: encoded, height: 1, width: 1)
        XCTAssertEqual(decoded, mask)
    }

    func testRoundTrip_singlePixelBackground() throws {
        // 1x1 image, fully background
        let mask = [false]
        let encoded = RLEDecoder.encode(mask: mask, height: 1, width: 1)
        let decoded = try RLEDecoder.decode(counts: encoded, height: 1, width: 1)
        XCTAssertEqual(decoded, mask)
    }

    func testRoundTrip_allForeground() throws {
        let width = 10
        let height = 8
        let mask = [Bool](repeating: true, count: width * height)
        let encoded = RLEDecoder.encode(mask: mask, height: height, width: width)
        let decoded = try RLEDecoder.decode(counts: encoded, height: height, width: width)
        XCTAssertEqual(decoded, mask)
    }

    func testRoundTrip_allBackground() throws {
        let width = 10
        let height = 8
        let mask = [Bool](repeating: false, count: width * height)
        let encoded = RLEDecoder.encode(mask: mask, height: height, width: width)
        let decoded = try RLEDecoder.decode(counts: encoded, height: height, width: width)
        XCTAssertEqual(decoded, mask)
    }

    func testRoundTrip_checkerboard() throws {
        // A checkerboard pattern exercises the column-major transpose
        let width = 6
        let height = 4
        var mask = [Bool](repeating: false, count: width * height)
        for row in 0 ..< height {
            for col in 0 ..< width {
                mask[row * width + col] = (row + col) % 2 == 0
            }
        }

        let encoded = RLEDecoder.encode(mask: mask, height: height, width: width)
        let decoded = try RLEDecoder.decode(counts: encoded, height: height, width: width)
        XCTAssertEqual(decoded, mask)
    }

    func testRoundTrip_rectangleMask() throws {
        // A solid rectangle in a larger image — nontrivial real-world-like mask
        let width = 40
        let height = 30
        var mask = [Bool](repeating: false, count: width * height)

        // Fill a 20x10 rectangle starting at (5, 8)
        for row in 8 ..< 18 {
            for col in 5 ..< 25 {
                mask[row * width + col] = true
            }
        }

        let encoded = RLEDecoder.encode(mask: mask, height: height, width: width)
        let decoded = try RLEDecoder.decode(counts: encoded, height: height, width: width)
        XCTAssertEqual(decoded, mask)
    }

    func testRoundTrip_singleRowMask() throws {
        // Edge case: image with height 1
        let width = 20
        let height = 1
        var mask = [Bool](repeating: false, count: width)
        mask[5] = true
        mask[6] = true
        mask[7] = true
        mask[15] = true

        let encoded = RLEDecoder.encode(mask: mask, height: height, width: width)
        let decoded = try RLEDecoder.decode(counts: encoded, height: height, width: width)
        XCTAssertEqual(decoded, mask)
    }

    func testRoundTrip_singleColumnMask() throws {
        // Edge case: image with width 1
        let width = 1
        let height = 20
        var mask = [Bool](repeating: false, count: height)
        mask[3] = true
        mask[4] = true
        mask[10] = true

        let encoded = RLEDecoder.encode(mask: mask, height: height, width: width)
        let decoded = try RLEDecoder.decode(counts: encoded, height: height, width: width)
        XCTAssertEqual(decoded, mask)
    }

    // MARK: - Error Cases

    func testDecode_invalidCharacter_throws() {
        // Characters below ASCII 48 are invalid
        XCTAssertThrowsError(try RLEDecoder.decode(counts: "!0", height: 1, width: 1)) { error in
            guard case let RLEDecoder.DecodeError.invalidCharacter(char) = error else {
                XCTFail("Expected invalidCharacter, got \(error)")
                return
            }
            XCTAssertEqual(char, Character("!"))
        }
    }

    func testDecode_invalidHighCharacter_throws() {
        // Characters above ASCII 111 are invalid
        XCTAssertThrowsError(try RLEDecoder.decode(counts: "p", height: 1, width: 1)) { error in
            guard case let RLEDecoder.DecodeError.invalidCharacter(char) = error else {
                XCTFail("Expected invalidCharacter, got \(error)")
                return
            }
            XCTAssertEqual(char, Character("p"))
        }
    }

    func testDecode_incompleteSequence_throws() {
        // A character with the continuation bit set (bit 5) but no following character.
        // ASCII 48 + 0x20 = 80 = 'P'
        XCTAssertThrowsError(try RLEDecoder.decode(counts: "P", height: 1, width: 1)) { error in
            guard case RLEDecoder.DecodeError.incompleteSequence = error else {
                XCTFail("Expected incompleteSequence, got \(error)")
                return
            }
        }
    }

    func testDecode_dimensionMismatch_throws() {
        // Encode a 2x2 mask but try to decode as 3x3
        let mask: [Bool] = [true, false, false, true]
        let encoded = RLEDecoder.encode(mask: mask, height: 2, width: 2)

        XCTAssertThrowsError(try RLEDecoder.decode(counts: encoded, height: 3, width: 3)) { error in
            guard case let RLEDecoder.DecodeError.dimensionMismatch(expected, actual) = error else {
                XCTFail("Expected dimensionMismatch, got \(error)")
                return
            }
            XCTAssertEqual(expected, 9)
            XCTAssertEqual(actual, 4)
        }
    }

    func testDecode_emptyString_zeroSize() throws {
        // Empty string for 0x0 image should succeed (0 runs summing to 0)
        let decoded = try RLEDecoder.decode(counts: "", height: 0, width: 0)
        XCTAssertTrue(decoded.isEmpty)
    }

    func testDecode_emptyString_nonzeroSize_throws() {
        // Empty string but non-zero dimensions → mismatch
        XCTAssertThrowsError(try RLEDecoder.decode(counts: "", height: 2, width: 2)) { error in
            guard case RLEDecoder.DecodeError.dimensionMismatch = error else {
                XCTFail("Expected dimensionMismatch, got \(error)")
                return
            }
        }
    }

    // MARK: - Large Run Length (Multi-Byte LEB128)

    func testRoundTrip_largeRunLength() throws {
        // A 100x100 image that's all foreground forces a run length of 10000,
        // which requires multi-byte LEB128 encoding
        let width = 100
        let height = 100
        let mask = [Bool](repeating: true, count: width * height)

        let encoded = RLEDecoder.encode(mask: mask, height: height, width: width)
        // Verify the encoded string is non-trivial (multi-byte sequences present)
        XCTAssertTrue(encoded.count > 2, "Large run should produce multi-byte encoding")

        let decoded = try RLEDecoder.decode(counts: encoded, height: height, width: width)
        XCTAssertEqual(decoded, mask)
    }
}
