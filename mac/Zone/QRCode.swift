import CoreImage
import CoreImage.CIFilterBuiltins
import SwiftUI

/// A QR code as a grid of on and off squares.
struct QRMatrix: Equatable {
    let side: Int
    let dark: [Bool]

    /// Makes the code for some text. Nil when the text is too long for a QR code.
    static func make(_ text: String) -> QRMatrix? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let image = filter.outputImage else { return nil }
        let extent = image.extent.integral
        let side = Int(extent.width)
        guard side > 0, Int(extent.height) == side else { return nil }
        var pixels = [UInt8](repeating: 0, count: side * side * 4)
        CIContext(options: [.useSoftwareRenderer: true]).render(
            image,
            toBitmap: &pixels,
            rowBytes: side * 4,
            bounds: extent,
            format: .RGBA8,
            colorSpace: CGColorSpaceCreateDeviceRGB()
        )
        // The generator draws one pixel per square, black on white, with a one-square margin.
        let dark = (0..<(side * side)).map { pixels[$0 * 4] < 128 }
        return QRMatrix(side: side, dark: dark)
    }
}

/// A QR code on a cream card: ink squares on paper, the way a phone camera reads best,
/// with the card's hard-edged shadow under it.
struct QRCodeView: View {
    let text: String
    var size: CGFloat = 220
    @Environment(\.colorScheme) private var scheme
    @State private var matrix: QRMatrix?

    private let paper = Color(hex: 0xF1E9D2)
    private let ink = Color(hex: 0x262626)

    var body: some View {
        Canvas { context, canvas in
            guard let matrix else { return }
            // Three squares of quiet paper around the code (the generator adds one itself).
            let quiet = 2.0
            let cell = canvas.width / (Double(matrix.side) + quiet * 2)
            var path = Path()
            for y in 0..<matrix.side {
                for x in 0..<matrix.side where matrix.dark[y * matrix.side + x] {
                    path.addRect(CGRect(
                        x: (Double(x) + quiet) * cell,
                        y: (Double(y) + quiet) * cell,
                        width: cell + 0.3,
                        height: cell + 0.3
                    ))
                }
            }
            context.fill(path, with: .color(ink))
        }
        .frame(width: size, height: size)
        .background {
            // The shadow belongs to the card alone. Put on the whole view it would also fall
            // from every square of the code, and a camera cannot read that.
            RoundedRectangle(cornerRadius: 14, style: .continuous)
                .fill(paper)
                .shadow(color: .black.opacity(scheme == .dark ? 0.55 : 0.2), radius: 0, x: 4, y: 5)
        }
        .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(Color.black.opacity(0.25), lineWidth: 1))
        .task(id: text) { matrix = QRMatrix.make(text) }
        .accessibilityElement()
        .accessibilityLabel("QR code")
    }
}
