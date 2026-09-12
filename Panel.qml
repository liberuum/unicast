import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Ui
import "Model.js" as Model

Panel {
  id: root
  moduleName: "universal-cast"
  manageIpc: false

  property var anchorItem: null
  property var hostWidget: null
  property bool casting: false
  property bool connecting: false
  property bool buffering: false
  property bool playing: false
  property bool paused: false
  property bool streamActive: false
  property real position: 0
  property real duration: 0
  property bool busy: false
  property bool disconnecting: false
  property string connectedReceiver: ""
  property string connectedIp: ""
  property string lastReceiver: ""
  property string errorText: ""
  property var devices: []
  property bool showSettings: false
  property bool scanning: false
  property bool heroHover: false
  property bool castBackend: false
  property bool airplayBackend: false
  property bool ffmpegAvailable: true
  property string missingDependencies: ""
  // False until the omarchy-castd daemon answers; backendProbed tells the
  // first answer (of any kind) apart from "not asked yet".
  property bool backendInstalled: false
  property bool backendProbed: false
  property bool installLaunched: false
  onBackendInstalledChanged: if (backendInstalled) installLaunched = false
  property bool multicastDiscovery: true
  property bool cursorActive: false
  property string focusSection: "header"
  property int headerIndex: 0
  property int receiverIndex: 0
  property string selectedFile: ""
  property string selectedFileName: ""
  property bool scrubbing: false
  property real scrubPosition: 0
  // The receiver a click is connecting to, shown immediately while the daemon
  // is still probing the file and talking to the device.
  property string pendingIp: ""
  property string pendingName: ""
  readonly property bool pendingConnect: pendingIp !== "" && !casting
  // The verb the action process is currently running (pause, seek, set-boost…)
  // so every control can show progress until the daemon answers.
  property string actionVerb: ""
  readonly property bool actionRunning: actionProc.running
  readonly property string actionLabel: actionVerb === "pause" ? "Pausing…"
    : actionVerb === "resume" ? "Resuming…"
    : actionVerb === "seek" ? "Seeking…"
    : actionVerb === "disconnect" ? "Stopping…"
    : actionVerb === "set-boost" ? "Applying boost…"
    : actionVerb === "set-subtitle" ? "Switching subtitles…"
    : actionVerb === "save-multicast" ? "Saving…"
    : ""
  // Where a seek is heading, shown on the progress bar until the daemon confirms.
  property real pendingSeek: -1
  // Receiver volume (0-100, -1 unknown) and whether this receiver lets us set it.
  property int volume: -1
  property bool muted: false
  property bool volumeSupported: false
  property int pendingVolume: -1
  // Audio boost in dB: the saved setting, and what the running stream carries.
  property int audioBoost: 0
  property int streamBoost: 0
  readonly property var boostOptions: [{ value: "0", label: "Off" }, { value: "6", label: "+6 dB" }, { value: "12", label: "+12 dB" }]
  // Subtitle tracks the daemon found for the file, and the active one (-1 = off).
  property var subtitles: []
  property int subtitle: -1
  property bool subtitlesSupported: false
  readonly property var subtitleOptions: subtitleOptionsFor(subtitles)

  readonly property string pluginDir: Quickshell.env("HOME") + "/.config/omarchy/plugins/universal-cast"
  readonly property string helper: pluginDir + "/bin/omarchy-cast"
  readonly property color foreground: bar ? bar.foreground : Color.foreground
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property color urgent: bar ? bar.urgent : Color.urgent
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family

  function open() {
    scanning = true
    root.controller.show()
    refreshPanel()
  }
  function close() {
    root.controller.hide()
  }
  // The file picker takes focus, which closes this popout. Once the picker
  // returns, bring the panel back where the flow left off (device list, with
  // the chosen file selected) instead of making the user click the widget.
  function reopen() {
    if (opened) return
    showSettings = false
    root.controller.show()
    run(statusProc, ["status"])
    if (devices.length === 0) reloadReceivers()
  }
  function switchPanel(direction) {
    if (root.bar && typeof root.bar.switchPanelFrom === "function") return root.bar.switchPanelFrom(root.hostWidget || root, direction)
    return false
  }
  function run(process, args) {
    if (process.running) return
    if (process === actionProc) actionVerb = args[0] || ""
    process.command = [helper].concat(args)
    process.running = true
  }
  function safeRemoteText(value, maximumLength) {
    return Model.safeRemoteText(value, maximumLength)
  }
  function deviceKind(receiver) {
    if (!receiver) return "tv"
    var model = String(receiver.model || "").toLowerCase()
    var name = String(receiver.name || "").toLowerCase()
    if (receiver.protocol === "airplay" || model.indexOf("apple") >= 0 || name.indexOf("apple") >= 0) {
      return "apple"
    }
    if (model.indexOf("chromecast") >= 0 || model.indexOf("google tv") >= 0 || model.indexOf("android tv") >= 0) {
      return "chromecast"
    }
    return "tv"
  }
  function deviceIcon(receiver) {
    return root.deviceKind(receiver) === "apple" ? "󰀵" : "󰔂"
  }
  function fmtTime(sec) {
    sec = Math.max(0, Math.floor(sec || 0))
    var h = Math.floor(sec / 3600), m = Math.floor((sec % 3600) / 60), s = sec % 60
    function p(n) { return (n < 10 ? "0" : "") + n }
    return (h > 0 ? h + ":" + p(m) : "" + m) + ":" + p(s)
  }
  function refreshPanel() {
    run(depsProc, ["deps"])
    run(statusProc, ["status"])
    reloadReceivers()
  }
  function reloadReceivers() {
    errorText = ""
    scanning = true
    run(discoverProc, ["discover"])
  }
  function isBackendMissing(message) {
    return /not installed/i.test(String(message || ""))
  }
  function applyStatus(raw) {
    var data = Model.parseJson(raw, { ok: false, error: "Invalid response" })
    if (!data.ok) {
      if (isBackendMissing(data.error)) { backendProbed = true; backendInstalled = false; return }
      errorText = data.error || "Cast status unavailable"; return
    }
    backendProbed = true
    backendInstalled = true
    // Only set an error here; never clear one a just-finished action set (a
    // periodic status poll must not wipe it). Errors clear on the next action.
    if (data.error) errorText = data.error
    lastReceiver = data.lastReceiver || lastReceiver
    multicastDiscovery = data.multicastDiscovery !== false
    if (data.audioBoost !== undefined) audioBoost = parseInt(data.audioBoost) || 0
    var streams = data.streams || []
    connecting = false; buffering = false; playing = false; paused = false
    connectedReceiver = ""; connectedIp = ""; position = 0; duration = 0
    volumeSupported = false; volume = -1; muted = false; streamBoost = 0
    subtitles = []; subtitle = -1; subtitlesSupported = false
    if (streams.length > 0) {
      var s = streams[0]
      var st = s.state || ""
      connectedReceiver = safeRemoteText(s.device || s.device_ip || "Receiver", 160)
      connectedIp = s.device_ip || ""
      position = s.position || 0
      duration = s.duration || 0
      volumeSupported = s.volumeSupported === true
      volume = (typeof s.volume === "number") ? s.volume : -1
      muted = s.muted === true
      streamBoost = s.boost || 0
      subtitles = s.subtitles || []
      subtitle = (typeof s.subtitle === "number") ? s.subtitle : -1
      subtitlesSupported = s.subtitlesSupported === true
      connecting = (st === "connecting")
      buffering = (st === "buffering")
      playing = (st === "playing")
      paused = (st === "paused" || s.paused === true)
    }
    casting = connecting || buffering || playing || paused
    streamActive = casting
  }
  function applyDevices(raw) {
    var data = Model.parseJson(raw, { ok: false, error: "Invalid device list" })
    if (!data.ok) {
      if (isBackendMissing(data.error)) { backendProbed = true; backendInstalled = false; return }
      errorText = data.error || "Unable to find devices"; return
    }
    devices = data.devices || []
    if (receiverIndex >= devices.length) receiverIndex = Math.max(0, devices.length - 1)
  }
  function applyDeps(raw) {
    var data = Model.parseJson(raw, {})
    var backends = data.backends || {}
    castBackend = backends.cast === true
    airplayBackend = backends.airplay === true
    ffmpegAvailable = data.ffmpeg !== false
    missingDependencies = (data.missing || []).join(", ")
    backendProbed = true
    backendInstalled = data.ok === true && data.installed === true
  }
  function installBackend() {
    // Runs the setup script straight away in the user's default terminal, so
    // they only approve the package and firewall prompts; --hold keeps the
    // window open afterwards so the result stays readable.
    installLaunched = true
    Quickshell.execDetached(["xdg-terminal-exec", "--hold", "--title=UniCast setup", pluginDir + "/bin/omarchy-cast-setup"])
  }
  function togglePause() {
    if (actionProc.running) return
    // The daemon acts on the active session and reports the real paused state
    // back via status; no local guessing.
    run(actionProc, [paused ? "resume" : "pause"])
  }
  function connectReceiver(ip) {
    if (!ip || busy) return
    errorText = ""
    busy = true
    var target = null
    for (var i = 0; i < devices.length; i++) { if (devices[i].ip === ip) { target = devices[i]; break } }
    pendingIp = ip
    pendingName = safeRemoteText(target && target.name ? target.name : ip, 160)
    if (root.selectedFile.length > 0) run(actionProc, ["connect", ip, root.selectedFile])
    else run(actionProc, ["connect", ip])
  }
  function chooseMedia() {
    if (!pickFileProc.running) pickFileProc.running = true
  }
  function seekTo(seconds) {
    if (actionProc.running || !streamActive) return
    pendingSeek = seconds
    run(actionProc, ["seek", seconds.toFixed(2)])
  }
  function setVolume(level) {
    if (!streamActive) return
    level = Math.max(0, Math.min(100, Math.round(level)))
    volume = level
    // Coalesce fast slider/wheel changes: the latest level is sent once the
    // in-flight request returns.
    if (volumeProc.running) { pendingVolume = level; return }
    run(volumeProc, ["set-volume", String(level)])
  }
  function toggleMute() {
    if (volumeProc.running || !streamActive) return
    muted = !muted
    run(volumeProc, ["set-mute", muted ? "true" : "false"])
  }
  function subtitleOptionsFor(tracks) {
    var options = [{ value: "off", label: "Off" }]
    for (var i = 0; i < (tracks || []).length; i++) {
      var track = tracks[i]
      options.push({ value: String(track.id), label: safeRemoteText(track.label || ("Track " + track.id), 60) })
    }
    return options
  }
  function setSubtitle(value) {
    if (actionProc.running || !streamActive) return
    errorText = ""
    subtitle = value === "off" ? -1 : parseInt(value)
    run(actionProc, ["set-subtitle", String(value)])
  }
  function setBoost(level) {
    if (actionProc.running || level === audioBoost) return
    errorText = ""
    audioBoost = level
    busy = true
    // The daemon saves the level and, when casting, re-serves the file with
    // the new gain from the current position.
    run(actionProc, ["set-boost", String(level)])
  }
  function disconnect() {
    if (actionProc.running || disconnecting) return
    errorText = ""
    busy = true
    disconnecting = true
    disconnectStartTimer.restart()
  }
  function toggleCast() {
    if (streamActive) disconnect()
    else if (lastReceiver) connectReceiver(lastReceiver)
    else errorText = "Choose a device first"
  }
  function addManualIp(ip) {
    if (!ip) return
    errorText = ""
    scanning = true
    // add-ip returns the discover-shaped {ok,devices}, so it must be applied by
    // applyDevices (via discoverProc), not applyStatus.
    run(discoverProc, ["add-ip", ip])
  }
  function saveMulticast(enabled) { run(actionProc, ["save-multicast", enabled ? "true" : "false"]) }
  function moveCursor(dx, dy) {
    cursorActive = true
    if (dy !== 0) {
      if (focusSection === "header" && dy > 0) {
        focusSection = devices.length > 0 ? "receivers" : "settings"
      } else if (focusSection === "receivers") {
        if (dy < 0 && receiverIndex <= 0) focusSection = "header"
        else if (dy > 0 && receiverIndex >= devices.length - 1) focusSection = "settings"
        else receiverIndex = Math.max(0, Math.min(devices.length - 1, receiverIndex + dy))
      } else if (focusSection === "settings" && dy < 0) {
        focusSection = devices.length > 0 ? "receivers" : "header"
      }
    }
    if (dx !== 0 && focusSection === "header") headerIndex = 0
    if (focusSection === "receivers") receiverList.positionViewAtIndex(receiverIndex, ListView.Contain)
  }
  function activateCursor() {
    cursorActive = true
    if (focusSection === "header") {
      reloadReceivers()
    } else if (focusSection === "receivers") {
      var receiver = devices[receiverIndex]
      if (streamActive) disconnect()
      else if (receiver) connectReceiver(receiver.ip)
    } else if (focusSection === "settings") {
      showSettings = !showSettings
    }
  }

  onOpenedChanged: if (opened) refreshPanel()
  Component.onCompleted: run(statusProc, ["status"])

  Timer {
    id: statusTimer
    interval: 3000
    running: true
    repeat: true
    onTriggered: {
      root.run(statusProc, ["status"])
      // While the backend is missing, keep asking so the view flips on its own
      // as soon as the setup script has finished.
      if (!root.backendInstalled) root.run(depsProc, ["deps"])
    }
  }
  Timer {
    id: disconnectStartTimer
    interval: 50
    repeat: false
    onTriggered: {
      if (actionProc.running) { root.disconnecting = false; return }
      root.run(actionProc, ["disconnect"])
    }
  }
  Timer {
    id: refreshTimer
    interval: 700
    repeat: true
    running: root.busy
    onTriggered: root.run(statusProc, ["status"])
  }
  Process {
    id: depsProc
    stdout: StdioCollector { id: depsOut }
    onExited: root.applyDeps(depsOut.text)
  }
  Process {
    id: statusProc
    stdout: StdioCollector { id: statusOut }
    onExited: root.applyStatus(statusOut.text)
  }
  Process {
    id: discoverProc
    stdout: StdioCollector { id: discoverOut }
    onExited: {
      root.scanning = false
      root.applyDevices(discoverOut.text)
    }
  }
  Process {
    id: actionProc
    stdout: StdioCollector { id: actionOut }
    onExited: {
      root.busy = false
      root.disconnecting = false
      root.applyStatus(actionOut.text)
      root.pendingIp = ""
      root.pendingName = ""
      root.pendingSeek = -1
      root.actionVerb = ""
      // Skip the immediate status re-poll if the action reported an error, so
      // the {ok:true} status response does not wipe the error message.
      if (root.errorText === "") root.run(statusProc, ["status"])
    }
  }
  Process {
    id: volumeProc
    stdout: StdioCollector { id: volumeOut }
    onExited: {
      root.applyStatus(volumeOut.text)
      if (root.pendingVolume >= 0) {
        var next = root.pendingVolume
        root.pendingVolume = -1
        Qt.callLater(function() { root.run(volumeProc, ["set-volume", String(next)]) })
      }
    }
  }
  Process { id: idleProc }
  Process {
    id: pickFileProc
    command: ["omarchy", "file", "select", "--title", "Choose media to cast", "--extensions", "mp4 mkv m4v mov avi webm ts m2ts mpg mpeg mp3 flac m4a aac ogg wav opus"]
    stdout: StdioCollector { id: pickFileOut }
    onExited: {
      var picked = pickFileOut.text.trim()
      if (picked.length > 0) {
        root.selectedFile = picked
        root.selectedFileName = picked.split("/").pop()
      }
      // Let the dialog window disappear first, then reopen the popout.
      reopenTimer.restart()
    }
  }
  Timer {
    id: reopenTimer
    interval: 150
    repeat: false
    onTriggered: root.reopen()
  }

  KeyboardPanel {
    id: panel
    anchorItem: root.anchorItem
    owner: root.hostWidget || root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: fittedContentWidth(Style.space(460))
    contentHeight: fittedContentHeight(contentColumn.implicitHeight, Style.space(600))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      blocked: manualIpInput.activeFocus
      onMoveRequested: function(dx, dy) { root.moveCursor(dx, dy) }
      onActivateRequested: root.activateCursor()
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
      onTextKey: function(text) {
        if (text === "r" || text === "R") root.reloadReceivers()
        else if (text === "w" || text === "W") root.toggleCast()
      }

      Flickable {
        anchors.fill: parent
        contentWidth: width
        contentHeight: contentColumn.implicitHeight
        clip: true
        ColumnLayout {
          id: contentColumn
          width: parent.width
          spacing: Style.space(10)

          RowLayout {
            Layout.fillWidth: true
            spacing: Style.space(12)
            Item {
              Layout.preferredWidth: Style.space(40)
              Layout.preferredHeight: Style.space(40)
              BorderSurface { anchors.fill: parent; color: "transparent"; radius: Style.cornerRadius; visible: root.heroHover; borderSpec: Border.controlSpec("hover-cursor", root.foreground, Color.accent) }
              CastIcon { anchors.fill: parent; active: root.casting || root.pendingConnect; foreground: (root.casting || root.pendingConnect) ? root.foreground : root.dim }
              MouseArea { anchors.fill: parent; hoverEnabled: true; cursorShape: Qt.PointingHandCursor; onContainsMouseChanged: root.heroHover = containsMouse; onClicked: root.toggleCast() }
            }
            ColumnLayout {
              Layout.fillWidth: true
              spacing: Style.space(2)
              Text { Layout.fillWidth: true; text: "UniCast"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.title; font.bold: true; elide: Text.ElideRight }
              Text { Layout.fillWidth: true; text: root.disconnecting ? "DISCONNECTING…" : (root.pendingConnect ? "CONNECTING TO " + root.pendingName.toUpperCase() : (root.actionLabel !== "" && root.actionVerb !== "disconnect" ? root.actionLabel.toUpperCase() : (root.scanning ? "SCANNING FOR DEVICES" : (root.connecting ? "CONNECTING TO " + root.connectedReceiver.toUpperCase() : (root.buffering ? "BUFFERING ON " + root.connectedReceiver.toUpperCase() : (root.paused ? "PAUSED ON " + root.connectedReceiver.toUpperCase() : (root.playing ? "PLAYING ON " + root.connectedReceiver.toUpperCase() : (root.backendProbed && !root.backendInstalled ? "BACKEND NOT INSTALLED" : (root.devices.length === 0 ? "NO DEVICES FOUND" : "NOT CASTING"))))))))); textFormat: Text.PlainText; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; elide: Text.ElideRight }
            }
            Button {
              iconText: "󰑐"
              iconSpinning: root.scanning
              tooltipText: root.scanning ? "Scanning…" : "Reload devices"
              foreground: root.foreground
              fontFamily: root.fontFamily
              iconSize: Style.font.subtitle * 1.5
              horizontalPadding: Style.space(5)
              verticalPadding: Style.space(2)
              hasCursor: root.cursorActive && root.focusSection === "header" && root.headerIndex === 0
              onHovered: function(on) { if (on) { root.cursorActive = false; root.focusSection = "header"; root.headerIndex = 0 } }
              onClicked: root.reloadReceivers()
            }
          }

          Text { visible: root.errorText !== ""; Layout.fillWidth: true; text: root.errorText; textFormat: Text.PlainText; color: root.urgent; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }

          ColumnLayout {
            visible: root.casting
            Layout.fillWidth: true
            spacing: Style.space(6)
            RowLayout {
              visible: root.duration > 0 && (root.playing || root.paused)
              Layout.fillWidth: true
              spacing: Style.space(8)
              Text { text: root.fmtTime(root.scrubbing ? root.scrubPosition : (root.pendingSeek >= 0 ? root.pendingSeek : root.position)); color: root.pendingSeek >= 0 && !root.scrubbing ? root.foreground : root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
              Item {
                id: scrubTrack
                Layout.fillWidth: true
                implicitHeight: Math.max(scrubBar.height, Style.space(18))
                // Mouse x over the track while hovering or dragging (-1 otherwise),
                // so the timestamp under the pointer can be shown before a click.
                property real hoverX: -1
                readonly property bool showHover: (scrubArea.containsMouse || root.scrubbing) && hoverX >= 0 && root.duration > 0
                readonly property real hoverTime: root.scrubbing ? root.scrubPosition : scrubArea.positionAt(hoverX)
                readonly property real hoverMarkerX: Math.max(0, Math.min(width, hoverX))
                Rectangle {
                  id: scrubBar
                  anchors.verticalCenter: parent.verticalCenter
                  width: parent.width
                  height: scrubArea.containsMouse || root.scrubbing ? 5 : 3
                  radius: height / 2
                  color: Qt.darker(root.foreground, 3.0)
                  Rectangle {
                    width: parent.width * Math.max(0, Math.min(1, root.duration > 0 ? (root.scrubbing ? root.scrubPosition : (root.pendingSeek >= 0 ? root.pendingSeek : root.position)) / root.duration : 0))
                    height: parent.height; radius: parent.radius; color: root.foreground
                  }
                }
                Rectangle {
                  visible: scrubTrack.showHover
                  x: Math.max(0, Math.min(parent.width - width, scrubTrack.hoverMarkerX - width / 2))
                  width: 2
                  height: scrubBar.height + Style.space(6)
                  radius: 1
                  anchors.verticalCenter: parent.verticalCenter
                  color: root.foreground
                }
                Rectangle {
                  id: hoverLabel
                  visible: scrubTrack.showHover
                  z: 10
                  x: Math.max(0, Math.min(parent.width - width, scrubTrack.hoverMarkerX - width / 2))
                  y: -height - Style.space(4)
                  width: hoverLabelText.implicitWidth + Style.space(12)
                  height: hoverLabelText.implicitHeight + Style.space(6)
                  radius: Style.cornerRadius
                  color: Color.tooltip.background
                  border.width: 1
                  border.color: Color.tooltip.border
                  Text {
                    id: hoverLabelText
                    anchors.centerIn: parent
                    text: root.fmtTime(scrubTrack.hoverTime)
                    color: Color.tooltip.text
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.bodySmall
                  }
                }
                MouseArea {
                  id: scrubArea
                  anchors.fill: parent
                  hoverEnabled: true
                  cursorShape: Qt.PointingHandCursor
                  function positionAt(mx) {
                    var fraction = Math.max(0, Math.min(1, mx / width))
                    return fraction * root.duration
                  }
                  onEntered: function() { scrubTrack.hoverX = mouseX }
                  onExited: function() { if (!root.scrubbing) scrubTrack.hoverX = -1 }
                  onPressed: function(m) { scrubTrack.hoverX = m.x; root.scrubbing = true; root.scrubPosition = positionAt(m.x) }
                  onPositionChanged: function(m) { scrubTrack.hoverX = m.x; if (pressed) root.scrubPosition = positionAt(m.x) }
                  onReleased: function(m) {
                    root.scrubPosition = positionAt(m.x)
                    root.scrubbing = false
                    if (!containsMouse) scrubTrack.hoverX = -1
                    root.seekTo(root.scrubPosition)
                  }
                  onCanceled: { root.scrubbing = false; scrubTrack.hoverX = -1 }
                }
              }
              Text { text: root.fmtTime(root.duration); color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            }
            RowLayout {
              visible: root.volumeSupported && (root.playing || root.paused || root.buffering)
              Layout.fillWidth: true
              spacing: Style.space(6)
              Button {
                iconText: root.muted ? "󰖁" : (root.volume >= 60 ? "󰕾" : (root.volume > 0 ? "󰖀" : "󰕿"))
                tooltipText: root.muted ? "Unmute the receiver" : "Mute the receiver"
                foreground: root.muted ? root.dim : root.foreground
                fontFamily: root.fontFamily
                iconSize: Style.font.subtitle * 1.3
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(2)
                onClicked: root.toggleMute()
              }
              PanelSlider {
                Layout.fillWidth: true
                bar: root.bar
                minimum: 0
                maximum: 100
                step: 5
                integer: true
                value: Math.max(0, root.volume)
                opacity: root.muted ? 0.5 : 1.0
                onReleased: function(v) { root.setVolume(v) }
                onRightClicked: root.toggleMute()
              }
              Text { Layout.preferredWidth: Style.space(36); horizontalAlignment: Text.AlignRight; text: root.volume >= 0 ? root.volume + "%" : "–"; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall }
            }
            RowLayout {
              visible: root.subtitlesSupported && root.subtitles.length > 0
              Layout.fillWidth: true
              spacing: Style.space(8)
              Text { Layout.fillWidth: true; text: "Subtitles"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.body }
              Dropdown {
                showLabel: false
                opacity: root.actionVerb === "set-subtitle" ? 0.6 : 1.0
                options: root.subtitleOptions
                value: root.subtitle >= 0 ? String(root.subtitle) : "off"
                foreground: root.foreground
                fontFamily: root.fontFamily
                onChanged: function(v) { root.setSubtitle(v) }
              }
            }
            RowLayout {
              Layout.fillWidth: true
              spacing: Style.space(8)
              Button { visible: root.playing || root.paused; Layout.fillWidth: true; text: root.actionVerb === "pause" ? "Pausing…" : (root.actionVerb === "resume" ? "Resuming…" : (root.paused ? "Resume" : "Pause")); opacity: root.actionRunning ? 0.6 : 1.0; bordered: true; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: root.togglePause() }
              Button { Layout.fillWidth: true; text: root.disconnecting || root.actionVerb === "disconnect" ? "Stopping…" : "Stop"; opacity: root.disconnecting || root.actionRunning ? 0.6 : 1.0; bordered: true; foreground: root.urgent; fontFamily: root.fontFamily; onClicked: root.disconnect() }
            }
            BoostRow {
              hint: root.audioBoost > 0
                ? "Soundtrack re-encoded +" + root.audioBoost + " dB with a limiter; changing it restarts the stream where it is."
                : "Off: the soundtrack is passed through untouched."
            }
          }

          ColumnLayout {
            visible: !root.showSettings && root.backendProbed && !root.backendInstalled
            Layout.fillWidth: true
            spacing: Style.space(8)
            PanelSeparator { Layout.fillWidth: true; foreground: root.foreground }
            PanelSectionHeader { text: "ONE MORE STEP"; foreground: root.foreground; fontFamily: root.fontFamily }
            Text { Layout.fillWidth: true; text: "The widget is installed, but its backend (omarchy-castd) is not built yet. The button opens your terminal and runs the setup: it installs the Rust toolchain, cmake and ffmpeg if they are missing, builds the daemon (a minute or two), and starts it as a user service. You only approve the prompts."; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
            Button { Layout.fillWidth: true; text: root.installLaunched ? "Setup is running in your terminal…" : "Build and install the backend"; opacity: root.installLaunched ? 0.7 : 1.0; bordered: true; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: root.installBackend() }
            Text { visible: root.installLaunched; Layout.fillWidth: true; text: "This panel switches to the device list by itself once the daemon answers."; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
          }

          ColumnLayout {
            visible: !root.showSettings && (root.backendInstalled || !root.backendProbed)
            Layout.fillWidth: true
            spacing: Style.space(8)
            PanelSeparator { Layout.fillWidth: true; foreground: root.foreground }
            PanelSectionHeader { text: "DEVICES"; foreground: root.foreground; fontFamily: root.fontFamily }
            Text { visible: root.devices.length === 0 && !root.scanning; Layout.fillWidth: true; text: "No receivers found. Make sure the TV is on and on this network, or add one by IP in Settings."; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
            Text { visible: root.devices.length > 0; Layout.fillWidth: true; text: root.selectedFile.length > 0 ? "Click a device to cast the selected file." : "Click a device to cast what your media player is playing — or choose a media file below."; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
            RowLayout {
              id: mediaRow
              visible: root.devices.length > 0
              Layout.fillWidth: true
              spacing: Style.space(6)
              // The shell Button never elides its label, so a long file name
              // would run under the Clear button. Measure the room left for the
              // label and elide the name in the middle to fit it.
              TextMetrics {
                id: mediaMetrics
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                elide: Text.ElideMiddle
                elideWidth: Math.max(Style.space(80), mediaRow.width - (clearMediaButton.visible ? clearMediaButton.implicitWidth + mediaRow.spacing : 0) - Style.space(40))
                text: "Media: " + root.selectedFileName
              }
              Button {
                Layout.fillWidth: true
                text: root.selectedFileName.length > 0 ? mediaMetrics.elidedText : "Choose media to play"
                tooltipText: root.selectedFileName.length > 0 ? root.selectedFileName : ""
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.chooseMedia()
              }
              Button { id: clearMediaButton; visible: root.selectedFile.length > 0; text: "Clear"; bordered: true; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: { root.selectedFile = ""; root.selectedFileName = "" } }
            }
            ListView {
              id: receiverList
              Layout.fillWidth: true
              Layout.preferredHeight: Math.min(contentHeight, Style.space(260))
              visible: root.devices.length > 0
              clip: true
              spacing: Style.space(4)
              model: root.devices
              delegate: ReceiverRow {
                required property var modelData
                required property int index
                width: ListView.view.width
                receiver: modelData
                listIndex: index
              }
            }
            Button { Layout.fillWidth: true; text: "Settings"; bordered: true; hasCursor: root.cursorActive && root.focusSection === "settings"; foreground: root.foreground; fontFamily: root.fontFamily; onHovered: function(on) { if (on) { root.cursorActive = false; root.focusSection = "settings" } }; onClicked: root.showSettings = true }
          }

          ColumnLayout {
            visible: root.showSettings
            Layout.fillWidth: true
            spacing: Style.space(8)
            PanelSeparator { Layout.fillWidth: true; foreground: root.foreground }
            RowLayout {
              Layout.fillWidth: true
              PanelSectionHeader { Layout.fillWidth: true; text: "SETTINGS"; foreground: root.foreground; fontFamily: root.fontFamily }
              Button { text: "Back"; bordered: true; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: root.showSettings = false }
            }

            PanelSectionHeader { text: "BACKEND"; foreground: root.foreground; fontFamily: root.fontFamily }
            Text { Layout.fillWidth: true; text: root.backendInstalled ? ("omarchy-castd is installed and running. DLNA, Google Cast and AirPlay receivers are supported." + (root.ffmpegAvailable ? "" : "\nMissing: " + root.missingDependencies + ". Audio boost, subtitles and transcoding need ffmpeg.")) : "The omarchy-castd backend is not installed yet. The button below installs the Rust toolchain, cmake and ffmpeg if needed, builds the daemon from the plugin checkout, and starts it as a user service."; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
            Button { visible: !root.backendInstalled || !root.ffmpegAvailable; Layout.fillWidth: true; text: root.backendInstalled ? "Install missing dependencies" : "Build and install the backend"; bordered: true; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: root.installBackend() }

            PanelSectionHeader { text: "AUDIO"; foreground: root.foreground; fontFamily: root.fontFamily }
            BoostRow { hint: "Movie mixes are often 8 to 15 dB quieter than TV and streaming apps. Boost re-encodes the soundtrack louder before it reaches the receiver, with a limiter against clipping and a dialogue-forward stereo downmix for 5.1. It applies to a running cast right away." }

            PanelSectionHeader { text: "ADD A DEVICE BY IP"; foreground: root.foreground; fontFamily: root.fontFamily }
            Text { Layout.fillWidth: true; text: "For a TV that does not show up automatically, enter its IP address."; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
            RowLayout {
              Layout.fillWidth: true
              spacing: Style.space(6)
              TextField { id: manualIpInput; Layout.fillWidth: true; placeholderText: "192.168.1.x"; onAccepted: { root.addManualIp(text); text = "" } }
              Button { text: "Add"; bordered: true; foreground: root.foreground; fontFamily: root.fontFamily; onClicked: { root.addManualIp(manualIpInput.text); manualIpInput.text = "" } }
            }

            ColumnLayout {
              Layout.fillWidth: true
              spacing: Style.space(2)
              RowLayout {
                Layout.fillWidth: true
                Text { Layout.fillWidth: true; text: "Scan the whole network (multicast)"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.body }
                ToggleSwitch {
                  checked: root.multicastDiscovery
                  foreground: root.foreground
                  onToggled: {
                    root.multicastDiscovery = !root.multicastDiscovery
                    root.saveMulticast(root.multicastDiscovery)
                  }
                }
              }
              Text { Layout.fillWidth: true; text: "Also send a broadcast probe to find DLNA devices that do not announce themselves over mDNS. Turn off if your firewall blocks it."; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
            }
          }
        }
      }
    }
  }

  component BoostRow: ColumnLayout {
    id: boostRow
    property string hint: ""
    Layout.fillWidth: true
    spacing: Style.space(3)
    RowLayout {
      Layout.fillWidth: true
      spacing: Style.space(8)
      Text { Layout.fillWidth: true; text: "Audio boost"; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.body }
      ButtonGroup {
        opacity: root.actionVerb === "set-boost" ? 0.6 : 1.0
        options: root.boostOptions
        value: String(root.audioBoost)
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        focusable: false
        onChanged: function(v) { root.setBoost(parseInt(v)) }
      }
    }
    Text { visible: boostRow.hint !== ""; Layout.fillWidth: true; text: boostRow.hint; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; wrapMode: Text.WordWrap }
  }

  component CastIcon: Canvas {
    id: castIcon
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
      ctx.lineWidth = Math.max(1.5, w * 0.055)
      ctx.lineCap = "round"
      ctx.lineJoin = "round"
      ctx.strokeStyle = castIcon.foreground
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
      ctx.fillStyle = castIcon.foreground
      ctx.beginPath(); ctx.arc(cx, cy, Math.max(1.0, w * 0.045), 0, 2 * Math.PI); ctx.fill()
    }
  }

  component ReceiverRow: CursorSurface {
    id: receiverRow
    required property var receiver
    required property int listIndex
    readonly property bool active: root.connectedIp === (receiver ? receiver.ip : "")
    readonly property bool pending: root.pendingConnect && receiver && root.pendingIp === receiver.ip
    readonly property bool unavailable: root.busy || root.casting || root.connecting
    property bool hovered: false
    width: parent ? parent.width : 0
    implicitHeight: rowBody.implicitHeight + Style.space(12)
    foreground: root.foreground
    current: active || pending
    hasCursor: hovered || (root.cursorActive && root.focusSection === "receivers" && root.receiverIndex === listIndex)

    BorderSurface {
      anchors.fill: parent
      visible: receiverRow.hovered
      color: "transparent"
      radius: Style.cornerRadius
      borderSpec: Border.controlSpec("hover-cursor", root.foreground, Color.accent)
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      cursorShape: unavailable ? Qt.ArrowCursor : Qt.PointingHandCursor
      onContainsMouseChanged: {
        receiverRow.hovered = containsMouse
        if (containsMouse) {
          root.cursorActive = false
          root.focusSection = "receivers"
          root.receiverIndex = listIndex
        }
      }
      onClicked: if (!unavailable && receiver) root.connectReceiver(receiver.ip)
    }

    Item {
      id: rowBody
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.top: parent.top
      anchors.leftMargin: Style.space(10)
      anchors.rightMargin: Style.space(10)
      anchors.topMargin: Style.space(6)
      height: implicitHeight
      implicitHeight: Math.max(receiverIcon.height, receiverInfo.implicitHeight, receiverState.height)
      Item {
        id: receiverIcon
        width: Style.space(20)
        height: Style.space(20)
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        Text {
          anchors.fill: parent
          visible: root.deviceKind(receiver) !== "chromecast"
          text: root.deviceIcon(receiver)
          color: active ? root.foreground : root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.title
          horizontalAlignment: Text.AlignHCenter
          verticalAlignment: Text.AlignVCenter
        }
        CastIcon {
          anchors.fill: parent
          visible: root.deviceKind(receiver) === "chromecast"
          foreground: active ? root.foreground : root.dim
        }
      }
      Column {
        id: receiverInfo
        anchors.left: receiverIcon.right
        anchors.leftMargin: Style.space(10)
        anchors.right: receiverState.left
        anchors.rightMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)
        Text { width: parent.width; text: receiver ? root.safeRemoteText(receiver.name, 160) : ""; textFormat: Text.PlainText; color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.body; elide: Text.ElideRight }
        Text { width: parent.width; text: Model.deviceSubtitle(receiver); textFormat: Text.PlainText; color: root.dim; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; elide: Text.ElideRight }
      }
      Row {
        id: receiverState
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(6)
        Text {
          visible: receiverRow.pending
          text: "󰑐"
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall * 1.2
          anchors.verticalCenter: parent.verticalCenter
          transformOrigin: Item.Center
          RotationAnimation on rotation { from: 0; to: 360; duration: 900; loops: Animation.Infinite; running: receiverRow.pending }
        }
        Text { text: active ? "Casting" : (receiverRow.pending ? "Connecting…" : ""); color: root.foreground; font.family: root.fontFamily; font.pixelSize: Style.font.bodySmall; anchors.verticalCenter: parent.verticalCenter }
      }
    }
  }
}
