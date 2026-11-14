// SPDX-License-Identifier: GPL-2.0-or-later
// Menu bar app for xbox-dongle: runs the bundled driver, shows its state.

import AppKit
import ServiceManagement

// MARK: - Driver process

/// Runs `xbox-dongle --json serve <firmware dir>` and decodes its events.
final class Driver {
    var onEvent: (([String: Any]) -> Void)?
    var onExit: ((Int32) -> Void)?

    private var process: Process?
    private var stdin: FileHandle?
    private var pending = Data()
    private let log: FileHandle?

    static let supportDir = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/xbox-dongle")
    static let firmwareDir = supportDir.appendingPathComponent("firmware")
    static let logURL = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Logs/xbox-dongle.log")

    init() {
        let fm = FileManager.default
        try? fm.createDirectory(at: Driver.firmwareDir, withIntermediateDirectories: true)
        try? fm.createDirectory(at: Driver.logURL.deletingLastPathComponent(), withIntermediateDirectories: true)
        // Start a fresh log when it gets big.
        if let size = (try? fm.attributesOfItem(atPath: Driver.logURL.path))?[.size] as? Int, size > 5_000_000 {
            try? fm.removeItem(at: Driver.logURL)
        }
        if !fm.fileExists(atPath: Driver.logURL.path) {
            fm.createFile(atPath: Driver.logURL.path, contents: nil)
        }
        log = try? FileHandle(forWritingTo: Driver.logURL)
        _ = try? log?.seekToEnd()
    }

    var isRunning: Bool { process?.isRunning ?? false }

    func start() {
        guard !isRunning else { return }
        let helper = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/xbox-dongle")
        let p = Process()
        p.executableURL = helper
        p.arguments = ["--json", "serve", Driver.firmwareDir.path]
        var env = ProcessInfo.processInfo.environment
        env["RUST_LOG"] = env["RUST_LOG"] ?? "info"
        p.environment = env

        let out = Pipe(), err = Pipe(), inp = Pipe()
        p.standardOutput = out
        p.standardError = err
        p.standardInput = inp
        out.fileHandleForReading.readabilityHandler = { [weak self] h in self?.consume(h.availableData) }
        err.fileHandleForReading.readabilityHandler = { [weak self] h in
            let d = h.availableData
            if !d.isEmpty { self?.writeLog(d) }
        }
        p.terminationHandler = { [weak self] proc in
            DispatchQueue.main.async { self?.onExit?(proc.terminationStatus) }
        }
        do {
            try p.run()
            process = p
            stdin = inp.fileHandleForWriting
            writeLog("--- driver started (pid \(p.processIdentifier)) ---\n".data(using: .utf8)!)
        } catch {
            writeLog("--- could not start driver: \(error) ---\n".data(using: .utf8)!)
            DispatchQueue.main.async { self.onExit?(-1) }
        }
    }

    func stop() {
        guard let p = process, p.isRunning else { return }
        p.terminate()
        p.waitUntilExit()
    }

    func send(_ command: String) {
        stdin?.write((command + "\n").data(using: .utf8)!)
    }

    private func writeLog(_ d: Data) {
        log?.write(d)
    }

    /// Split stdout into lines; JSON objects are events, the rest goes to the log.
    private func consume(_ d: Data) {
        guard !d.isEmpty else { return }
        pending.append(d)
        while let nl = pending.firstIndex(of: 0x0a) {
            let line = pending.subdata(in: pending.startIndex..<nl)
            pending.removeSubrange(pending.startIndex...nl)
            if line.first == UInt8(ascii: "{"),
               let obj = try? JSONSerialization.jsonObject(with: line) as? [String: Any] {
                DispatchQueue.main.async { self.onEvent?(obj) }
            } else {
                writeLog(line + Data([0x0a]))
            }
        }
    }
}

// MARK: - State

struct Controller {
    var mac: String
    var firmware: String?
    var batteryKind: Int?
    var batteryLevel: Int?
}

enum DriverState: Equatable {
    case stopped
    case waiting
    case starting(String)
    case ready
    case error(String)
}

// MARK: - App

final class AppDelegate: NSObject, NSApplicationDelegate, NSMenuDelegate {
    private let driver = Driver()
    private var statusItem: NSStatusItem!
    private var state: DriverState = .stopped
    private var pairing = false
    private var controllers: [Int: Controller] = [:]
    private var firmwareMissing: String?
    private var quitting = false
    private var blinkTimer: Timer?
    private var blinkOn = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        let menu = NSMenu()
        menu.delegate = self
        statusItem.menu = menu

        driver.onEvent = { [weak self] in self?.handle($0) }
        driver.onExit = { [weak self] code in self?.driverExited(code) }
        driver.start()
        updateIcon()
    }

    func applicationWillTerminate(_ notification: Notification) {
        quitting = true
        driver.stop()
    }

    // MARK: Events

    private func handle(_ e: [String: Any]) {
        let wcid = e["wcid"] as? Int ?? 0
        switch e["event"] as? String {
        case "waiting":
            state = .waiting
            pairing = false
            controllers.removeAll()
        case "starting":
            state = .starting(e["adapter"] as? String ?? "adaptateur")
            firmwareMissing = nil
            controllers.removeAll()
        case "ready":
            state = .ready
        case "pairing":
            pairing = e["on"] as? Bool ?? false
        case "connected":
            controllers[wcid] = Controller(mac: e["mac"] as? String ?? "?")
        case "controller":
            controllers[wcid]?.firmware = e["firmware"] as? String
        case "battery":
            controllers[wcid]?.batteryKind = e["kind"] as? Int
            controllers[wcid]?.batteryLevel = e["level"] as? Int
        case "disconnected":
            controllers[wcid] = nil
        case "stopped":
            state = .waiting
            pairing = false
            controllers.removeAll()
        case "error":
            if let pid = e["firmware_missing"] as? String { firmwareMissing = pid }
            state = .error(e["message"] as? String ?? "erreur")
        default:
            break
        }
        updateIcon()
    }

    private func driverExited(_ code: Int32) {
        guard !quitting else { return }
        state = .error("le driver s'est arrêté (code \(code)), relance…")
        pairing = false
        controllers.removeAll()
        updateIcon()
        DispatchQueue.main.asyncAfter(deadline: .now() + 3) { [weak self] in
            guard let self, !self.quitting else { return }
            self.driver.start()
        }
    }

    // MARK: Icon

    private func updateIcon() {
        if pairing {
            if blinkTimer == nil {
                blinkTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
                    self?.blinkOn.toggle()
                    self?.drawIcon()
                }
            }
        } else {
            blinkTimer?.invalidate()
            blinkTimer = nil
            blinkOn = false
        }
        drawIcon()
    }

    private func drawIcon() {
        guard let button = statusItem.button else { return }
        let symbol: String
        var dimmed = false
        switch state {
        case .error:
            symbol = "exclamationmark.triangle"
        case .stopped, .waiting:
            symbol = "gamecontroller"
            dimmed = true
        case .starting:
            symbol = "gamecontroller"
        case .ready:
            if pairing {
                symbol = blinkOn ? "gamecontroller.fill" : "gamecontroller"
            } else {
                symbol = controllers.isEmpty ? "gamecontroller" : "gamecontroller.fill"
            }
        }
        let image = NSImage(systemSymbolName: symbol, accessibilityDescription: statusText)
        image?.isTemplate = true
        button.image = image
        button.appearsDisabled = dimmed
        button.toolTip = "Xbox Dongle — \(statusText)"
    }

    private var statusText: String {
        switch state {
        case .stopped: return "driver arrêté"
        case .waiting: return "en attente de l'adaptateur"
        case .starting(let name): return "démarrage de \(name)…"
        case .ready:
            if pairing { return "appairage en cours" }
            switch controllers.count {
            case 0: return "prêt, aucune manette"
            case 1: return "1 manette connectée"
            default: return "\(controllers.count) manettes connectées"
            }
        case .error(let msg): return msg
        }
    }

    // MARK: Menu

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let title = NSMenuItem(title: "Xbox Dongle — \(statusText)", action: nil, keyEquivalent: "")
        title.isEnabled = false
        menu.addItem(title)

        for (wcid, c) in controllers.sorted(by: { $0.key < $1.key }) {
            var text = "Manette \(wcid)"
            if let level = c.batteryLevel, c.batteryKind != 0 {
                let names = ["critique", "faible", "moyenne", "pleine"]
                text += " — batterie \(level < names.count ? names[level] : "?")"
            }
            let item = NSMenuItem(title: text, action: nil, keyEquivalent: "")
            item.image = NSImage(systemSymbolName: "gamecontroller.fill", accessibilityDescription: nil)
            item.toolTip = "\(c.mac)\(c.firmware.map { ", firmware \($0)" } ?? "")"
            item.isEnabled = false
            menu.addItem(item)
        }

        menu.addItem(.separator())
        if let pid = firmwareMissing {
            menu.addItem(withTitle: "Télécharger le firmware (\(pid))…", action: #selector(downloadFirmware), keyEquivalent: "")
                .target = self
        }
        let pair = NSMenuItem(title: pairing ? "Appairage en cours…" : "Appairer une manette",
                              action: #selector(pairController), keyEquivalent: "p")
        pair.target = self
        pair.isEnabled = state == .ready && !pairing
        menu.addItem(pair)

        menu.addItem(.separator())
        let login = NSMenuItem(title: "Ouvrir à l'ouverture de session", action: #selector(toggleLogin), keyEquivalent: "")
        login.target = self
        login.state = SMAppService.mainApp.status == .enabled ? .on : .off
        menu.addItem(login)
        menu.addItem(withTitle: "Afficher le journal", action: #selector(showLog), keyEquivalent: "l").target = self
        menu.addItem(withTitle: "Redémarrer le driver", action: #selector(restartDriver), keyEquivalent: "r").target = self
        menu.addItem(.separator())
        menu.addItem(withTitle: "Quitter Xbox Dongle", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
    }

    @objc private func pairController() {
        driver.send("pair")
    }

    @objc private func toggleLogin() {
        do {
            if SMAppService.mainApp.status == .enabled {
                try SMAppService.mainApp.unregister()
            } else {
                try SMAppService.mainApp.register()
            }
        } catch {
            alert("Impossible de modifier l'ouverture automatique", "\(error.localizedDescription)")
        }
    }

    @objc private func showLog() {
        NSWorkspace.shared.open(Driver.logURL)
    }

    @objc private func restartDriver() {
        quitting = true
        driver.stop()
        quitting = false
        state = .stopped
        controllers.removeAll()
        pairing = false
        updateIcon()
        driver.start()
    }

    @objc private func downloadFirmware() {
        guard let pid = firmwareMissing else { return }
        let a = NSAlert()
        a.messageText = "Télécharger le firmware de l'adaptateur ?"
        a.informativeText = """
            Le firmware (environ 200 Ko) est téléchargé depuis le Microsoft Update Catalog \
            (catalog.s.download.windowsupdate.com). Il est soumis aux conditions d'utilisation \
            de Microsoft : https://www.microsoft.com/en-us/legal/terms-of-use
            """
        a.addButton(withTitle: "Télécharger")
        a.addButton(withTitle: "Annuler")
        NSApp.activate(ignoringOtherApps: true)
        guard a.runModal() == .alertFirstButtonReturn else { return }

        guard let script = Bundle.main.url(forResource: "fetch-firmware", withExtension: "sh") else { return }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/bash")
        p.arguments = [script.path, pid, Driver.firmwareDir.path]
        let errPipe = Pipe()
        p.standardError = errPipe
        p.terminationHandler = { proc in
            let msg = String(data: errPipe.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            DispatchQueue.main.async {
                if proc.terminationStatus != 0 {
                    self.alert("Échec du téléchargement du firmware", msg)
                }
                // On success the driver notices the file and starts by itself.
            }
        }
        try? p.run()
    }

    private func alert(_ title: String, _ text: String) {
        let a = NSAlert()
        a.messageText = title
        a.informativeText = text
        NSApp.activate(ignoringOtherApps: true)
        a.runModal()
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.setActivationPolicy(.accessory) // menu bar only, no Dock icon
app.run()
