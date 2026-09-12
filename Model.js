function parseJson(raw, fallback) {
  try { return JSON.parse(String(raw || "")) } catch (e) { return fallback }
}

function safeRemoteText(value, maximumLength) {
  var limit = maximumLength || 160
  return String(value || "")
    .replace(/[\x00-\x1f\x7f]/g, " ")
    .replace(/[<>]/g, "")
    .slice(0, limit)
}

function protocolLabel(p) {
  if (p === "dlna") return "DLNA"
  if (p === "cast") return "Chromecast"
  if (p === "airplay") return "AirPlay"
  return p ? String(p).toUpperCase() : ""
}

function deviceSubtitle(device) {
  if (!device) return ""
  var parts = []
  var proto = protocolLabel(device.protocol)
  if (proto) parts.push(proto)
  if (device.model) parts.push(safeRemoteText(device.model, 80))
  var addr = device.ip || device.host
  if (addr) parts.push(safeRemoteText(addr, 64))
  return parts.join(" · ")
}
