import QtQuick
import Quickshell
import qs.Commons
import qs.Ui

BarWidget {
  id: root
  moduleName: "universal-cast"
  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property bool casting: panelLoader.item ? panelLoader.item.casting === true : false

  readonly property bool opened: panelLoader.item ? panelLoader.item.opened === true : false
  readonly property bool popoutSwitchClosing: panelLoader.item ? panelLoader.item.popoutSwitchClosing === true : false

  function open() { if (panelLoader.item) panelLoader.item.open() }
  function close() { if (panelLoader.item) panelLoader.item.close() }
  function toggle() { if (panelLoader.item) panelLoader.item.toggle() }
  function closeForPopoutSwitch() { if (panelLoader.item) panelLoader.item.closeForPopoutSwitch() }
  function injectPanel() {
    if (!panelLoader.item) return
    panelLoader.item.bar = root.bar
    panelLoader.item.anchorItem = button
    panelLoader.item.hostWidget = root
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight
  onBarChanged: injectPanel()

  Loader {
    id: panelLoader
    active: true
    source: Qt.resolvedUrl("Panel.qml")
    visible: false
    onLoaded: { root.injectPanel(); Qt.callLater(root.injectPanel) }
  }

  // Use BarIconButton (like the built-in bar widgets) so the icon sits in the
  // standard icon slot with the same spacing as its neighbours.
  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    iconComponent: castIconComponent
    active: root.casting
    tooltipText: (panelLoader.item && panelLoader.item.connectedReceiver) ? "Casting to " + panelLoader.item.connectedReceiver : "UniCast"
    onPressed: function(b) { if (b === Qt.LeftButton) root.toggle() }
  }

  // While casting, the glyph takes the bar's active colour (the same one the
  // microphone widget uses while in use) instead of the plain foreground, so an
  // ongoing cast is visible at a glance; idle stays dimmed.
  Component {
    id: castIconComponent
    CastIcon {
      active: root.casting
      foreground: root.casting ? button.activeColor : Qt.darker(root.foreground, 1.28)
    }
  }

  // Cast glyph on a Canvas: a screen with the signal waves INSIDE its
  // lower-left corner. Sized by its parent (the icon canvas), no fixed size.
  component CastIcon: Canvas {
    id: icon
    property bool active: false
    property color foreground: "white"
    onActiveChanged: requestPaint()
    onForegroundChanged: requestPaint()
    onWidthChanged: requestPaint()
    onHeightChanged: requestPaint()
    onPaint: {
      var ctx = getContext("2d")
      ctx.reset()
      var w = width, h = height
      ctx.lineWidth = Math.max(1.25, w * 0.075)
      ctx.lineCap = "round"
      ctx.lineJoin = "round"
      ctx.strokeStyle = icon.foreground
      var x0 = w * 0.13, y0 = h * 0.16, x1 = w * 0.87, y1 = h * 0.74
      var rr = Math.min(w, h) * 0.10
      ctx.beginPath()
      ctx.moveTo(x0 + rr, y0)
      ctx.lineTo(x1 - rr, y0); ctx.arcTo(x1, y0, x1, y0 + rr, rr)
      ctx.lineTo(x1, y1 - rr); ctx.arcTo(x1, y1, x1 - rr, y1, rr)
      ctx.lineTo(x0 + rr, y1); ctx.arcTo(x0, y1, x0, y1 - rr, rr)
      ctx.lineTo(x0, y0 + rr); ctx.arcTo(x0, y0, x0 + rr, y0, rr)
      ctx.stroke()
      var cx = w * 0.24, cy = h * 0.63
      ctx.beginPath(); ctx.arc(cx, cy, w * 0.14, -Math.PI / 2, 0); ctx.stroke()
      ctx.beginPath(); ctx.arc(cx, cy, w * 0.26, -Math.PI / 2, 0); ctx.stroke()
      ctx.fillStyle = icon.foreground
      ctx.beginPath(); ctx.arc(cx, cy, Math.max(1.0, w * 0.045), 0, 2 * Math.PI); ctx.fill()
    }
  }
}
