// ProfileDocumentIO.swift — read / write a `VehicleProfileDocument` at a file URL.
//
// File I/O only. No UI, no AppKit: the open/save panels, the clamp notice and the error alert are
// W4's `ProfileDocumentView` (DESIGN.md §6); this file exists so that layer holds no parsing logic
// and so the same two calls can run in the hardware-free test harness (tests/run_tests.sh compiles
// Foundation-only sources — `VehicleProfile.swift` is one, this is another).
//
// Bytes on disk are EXACTLY `VehicleProfileDocument.encode(_:)` — sorted keys, pretty-printed, no
// trailing newline added here. That keeps DESIGN.md §0.2's promise ("equal documents are byte-equal")
// true for files as well as for `Data`: exporting the same profile twice gives identical files, and a
// reader that hashes or diffs exports gets no spurious churn. Decoding tolerates whatever a human
// editor did (JSONSerialization does not care about trailing whitespace), so an edited file still loads.

import Foundation

enum ProfileDocumentIO {

    /// Why a document could not be read. `Data(contentsOf:)` failures are already `LocalizedError`
    /// (CocoaError) and pass through untouched; decode failures are wrapped here because
    /// `VehicleProfileDocument.DocumentError` and Swift's `DecodingError` are NOT `LocalizedError` —
    /// `error.localizedDescription` on either renders as "The operation couldn't be completed.
    /// (… error 1.)", which is exactly the alert text that taught nobody anything the last time a
    /// pushed document was rejected (docs/carplay/04 B3). The reason string names the file, the
    /// JSON path and the mismatch, so a hand-edit is fixable from the alert alone.
    enum IOError: LocalizedError, Equatable {
        case malformed(file: String, reason: String)

        var errorDescription: String? {
            switch self {
            case let .malformed(file, reason):
                return "\(file) is not a usable vehicle profile: \(reason)"
            }
        }
    }

    /// Read and decode. Schema migrations and missing-key defaults (at every depth —
    /// `fillingMissingKeys`) happen inside `VehicleProfileDocument.decode`; a document from a NEWER
    /// app is refused there (`DocumentError.newerSchema`) rather than half-loaded, and that refusal
    /// surfaces here with the file name attached. "missing key" therefore now names only a field
    /// with no default (`BrandIcon`'s three, `ConnectorSpec.type`).
    static func read(from url: URL) throws -> VehicleProfileDocument {
        let data = try Data(contentsOf: url)
        do {
            return try VehicleProfileDocument.decode(data)
        } catch let e as VehicleProfileDocument.DocumentError {
            throw IOError.malformed(file: url.lastPathComponent, reason: e.description)
        } catch let e as DecodingError {
            throw IOError.malformed(file: url.lastPathComponent, reason: describe(e))
        } catch {
            throw IOError.malformed(file: url.lastPathComponent, reason: "\(error)")
        }
    }

    /// Encode and write atomically (temp file + rename), so a crash or a full disk mid-write leaves
    /// the previous export intact rather than a truncated JSON that the next Import refuses.
    static func write(_ doc: VehicleProfileDocument, to url: URL) throws {
        let data = try VehicleProfileDocument.encode(doc)
        try data.write(to: url, options: [.atomic])
    }

    /// Render a `DecodingError` as "path: what went wrong". The coding path is the JSON key path
    /// (`vehicle.display.panel.width`), which is what the human editing the file needs; the type
    /// name is Swift's, which is close enough (`Int`, `Bool`, `String`) to be self-explanatory.
    private static func describe(_ e: DecodingError) -> String {
        func path(_ ctx: DecodingError.Context) -> String {
            let p = ctx.codingPath.map(\.stringValue).joined(separator: ".")
            return p.isEmpty ? "(top level)" : p
        }
        switch e {
        case let .keyNotFound(key, ctx):
            return "\(path(ctx)): missing key '\(key.stringValue)'"
        case let .typeMismatch(type, ctx):
            return "\(path(ctx)): expected \(type)"
        case let .valueNotFound(type, ctx):
            return "\(path(ctx)): null where \(type) was expected"
        case let .dataCorrupted(ctx):
            return "\(path(ctx)): \(ctx.debugDescription)"
        @unknown default:
            return "\(e)"
        }
    }
}
