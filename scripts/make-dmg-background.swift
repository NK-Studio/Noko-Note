// DMG 창 배경 src-tauri/dmg/background.tiff 를 그린다(1x 660×400 + 2x 1320×800 을 한 TIFF 에).
// 왼쪽(170,190)에 Noko-Note.app, 오른쪽(490,190)에 Applications 바로가기가 놓이고
// (src-tauri/tauri.conf.json 의 bundle.macOS.dmg 좌표와 같다),
// 가운데 화살표와 아래 안내 문구가 끌어다 놓는 설치를 알려 준다.
// 사용: swift scripts/make-dmg-background.swift [출력 경로]  (배경을 고쳤을 때만, macOS 에서)
import AppKit

let W: CGFloat = 660, H: CGFloat = 400
let out = CommandLine.arguments.count > 1
  ? CommandLine.arguments[1]
  : URL(fileURLWithPath: #filePath).deletingLastPathComponent()
      .appendingPathComponent("../src-tauri/dmg/background.tiff").standardized.path

func render(scale: CGFloat) -> NSBitmapImageRep {
  let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W * scale), pixelsHigh: Int(H * scale),
                             bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
                             colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
  rep.size = NSSize(width: W, height: H) // 2x 는 같은 pt 크기에 픽셀만 두 배(144 dpi)
  NSGraphicsContext.saveGraphicsState()
  let bitmap = NSGraphicsContext(bitmapImageRep: rep)!
  // 위(0)에서 아래(H)로 내려가는 좌표를 쓴다(글자도 바로 서도록 flipped 컨텍스트로).
  bitmap.cgContext.translateBy(x: 0, y: H)
  bitmap.cgContext.scaleBy(x: 1, y: -1)
  let ctx = NSGraphicsContext(cgContext: bitmap.cgContext, flipped: true)
  NSGraphicsContext.current = ctx

  // 바탕: 위는 흰색, 아래로 아주 옅게 어두워진다.
  NSGradient(starting: NSColor(srgbRed: 1, green: 1, blue: 1, alpha: 1),
             ending: NSColor(srgbRed: 0.933, green: 0.933, blue: 0.945, alpha: 1))!
    .draw(in: NSRect(x: 0, y: 0, width: W, height: H), angle: 90)

  // 화살표: 점선 줄기 + 화살촉.
  let gray = NSColor(srgbRed: 0.6, green: 0.6, blue: 0.635, alpha: 1)
  gray.setStroke()
  let shaft = NSBezierPath()
  shaft.move(to: NSPoint(x: 268, y: 190)); shaft.line(to: NSPoint(x: 382, y: 190))
  shaft.lineWidth = 3; shaft.lineCapStyle = .round
  shaft.setLineDash([0.01, 10], count: 2, phase: 0)
  shaft.stroke()
  let head = NSBezierPath()
  head.move(to: NSPoint(x: 378, y: 175)); head.line(to: NSPoint(x: 394, y: 190)); head.line(to: NSPoint(x: 378, y: 205))
  head.lineWidth = 3; head.lineCapStyle = .round; head.lineJoinStyle = .round
  head.stroke()

  // 안내 문구(가운데 정렬).
  func text(_ s: String, y: CGFloat, size: CGFloat, weight: NSFont.Weight, color: NSColor) {
    let p = NSMutableParagraphStyle(); p.alignment = .center
    let a = NSAttributedString(string: s, attributes: [
      .font: NSFont.systemFont(ofSize: size, weight: weight), .foregroundColor: color, .paragraphStyle: p])
    a.draw(in: NSRect(x: 0, y: y, width: W, height: size * 1.6))
  }
  text("Noko-Note를 Applications 폴더로 끌어다 놓으세요", y: 332, size: 15, weight: .medium,
       color: NSColor(srgbRed: 0.227, green: 0.227, blue: 0.25, alpha: 1))
  text("Drag Noko-Note to the Applications folder to install", y: 356, size: 12, weight: .regular,
       color: NSColor(srgbRed: 0.557, green: 0.557, blue: 0.588, alpha: 1))

  ctx.flushGraphics()
  NSGraphicsContext.restoreGraphicsState()
  return rep
}

let img = NSImage(size: NSSize(width: W, height: H))
img.addRepresentation(render(scale: 1))
img.addRepresentation(render(scale: 2))
guard let tiff = img.tiffRepresentation(using: .lzw, factor: 0) else { fatalError("TIFF 를 만들지 못했습니다") }
try! FileManager.default.createDirectory(at: URL(fileURLWithPath: out).deletingLastPathComponent(),
                                         withIntermediateDirectories: true)
try! tiff.write(to: URL(fileURLWithPath: out))
print(out)
