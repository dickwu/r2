// Disposable Linux VM controller. All writable artifacts must belong to --vm-dir.
// JSON commands over <vm-dir>/control.sock: status, start, stop, request-stop, shutdown.
// `stop` is a guest power cut; no command operates on host power or arbitrary paths.
import Foundation
import Virtualization
import Darwin

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

func jsonData(_ value: [String: Any]) -> Data {
    var data = (try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])) ?? Data("{}".utf8)
    data.append(10)
    return data
}

func event(_ name: String, _ fields: [String: Any] = [:]) {
    var value = fields
    value["event"] = name
    value["at"] = ISO8601DateFormatter().string(from: Date())
    FileHandle.standardOutput.write(jsonData(value))
}

func sendAll(_ fd: Int32, _ bytes: UnsafeRawPointer, _ count: Int) -> Bool {
    var sent = 0
    while sent < count {
        let result = Darwin.write(fd, bytes.advanced(by: sent), count - sent)
        if result < 0 && errno == EINTR { continue }
        if result <= 0 { return false }
        sent += result
    }
    return true
}

func reply(_ fd: Int32, _ value: [String: Any]) {
    let data = jsonData(value)
    _ = data.withUnsafeBytes { sendAll(fd, $0.baseAddress!, data.count) }
    Darwin.close(fd)
}

func makeSocket(_ domain: Int32) -> Int32 {
    let fd = socket(domain, SOCK_STREAM, 0)
    if fd < 0 { fail("Cannot create owned listener: \(errno)") }
    var enabled: Int32 = 1
    setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, &enabled, socklen_t(MemoryLayout<Int32>.size))
    return fd
}

final class Runner: NSObject, VZVirtualMachineDelegate {
    let root: URL
    let owner: String
    let cpuCount: Int
    let memorySize: UInt64
    let osCachingMode: VZDiskImageCachingMode
    var machine: VZVirtualMachine?
    var changingState = false
    var bridges: [UUID: VZVirtioSocketConnection] = [:]
    var sshPort: UInt16 = 0
    var consoleInput: FileHandle?

    init(root: URL, cpuCount: Int, memoryGiB: Int, osCachingMode: VZDiskImageCachingMode) throws {
        self.root = root.standardizedFileURL.resolvingSymlinksInPath()
        self.cpuCount = cpuCount
        self.memorySize = UInt64(memoryGiB) * 1024 * 1024 * 1024
        self.osCachingMode = osCachingMode
        let marker = try JSONSerialization.jsonObject(with: Data(contentsOf: self.root.appendingPathComponent("ownership.json"))) as? [String: Any]
        guard marker?["kind"] as? String == "r2-disposable-vm", let owner = marker?["owner"] as? String,
              UUID(uuidString: owner) != nil, let claimedRoot = marker?["root"] as? String,
              URL(fileURLWithPath: claimedRoot, isDirectory: true).standardizedFileURL.resolvingSymlinksInPath().path == self.root.path else {
            throw NSError(domain: "VMAudit", code: 1, userInfo: [NSLocalizedDescriptionKey: "Missing or mismatched VM ownership marker"])
        }
        self.owner = owner
        super.init()
    }

    func owned(_ name: String) -> URL { root.appendingPathComponent(name) }

    func regular(_ name: String) throws -> URL {
        let url = owned(name)
        let properties = try url.resourceValues(forKeys: [.isRegularFileKey, .isSymbolicLinkKey])
        guard properties.isRegularFile == true, properties.isSymbolicLink != true else {
            throw NSError(domain: "VMAudit", code: 2, userInfo: [NSLocalizedDescriptionKey: "Not an owned regular artifact: \(name)"])
        }
        return url
    }

    func configuration() throws -> VZVirtualMachineConfiguration {
        let configuration = VZVirtualMachineConfiguration()
        configuration.cpuCount = cpuCount
        configuration.memorySize = memorySize
        let platform = VZGenericPlatformConfiguration()
        let identifierURL = owned("machine-id.bin")
        if FileManager.default.fileExists(atPath: identifierURL.path) {
            guard let identifier = VZGenericMachineIdentifier(dataRepresentation: try Data(contentsOf: identifierURL)) else {
                throw NSError(domain: "VMAudit", code: 3)
            }
            platform.machineIdentifier = identifier
        } else {
            platform.machineIdentifier = VZGenericMachineIdentifier()
            try platform.machineIdentifier.dataRepresentation.write(to: identifierURL, options: .atomic)
        }
        configuration.platform = platform
        let loader = VZEFIBootLoader()
        let nvram = owned("efi-vars.bin")
        loader.variableStore = FileManager.default.fileExists(atPath: nvram.path)
            ? VZEFIVariableStore(url: nvram)
            : try VZEFIVariableStore(creatingVariableStoreAt: nvram)
        configuration.bootLoader = loader
        configuration.storageDevices = try [("os.raw", false, "r2-audit-os"), ("audit.raw", false, "r2-audit-data"), ("seed.iso", true, "r2-audit-seed")].map { name, readOnly, identifier in
            let caching = name == "os.raw" ? osCachingMode : VZDiskImageCachingMode.uncached
            let attachment = try VZDiskImageStorageDeviceAttachment(url: regular(name), readOnly: readOnly, cachingMode: caching, synchronizationMode: .full)
            let disk = VZVirtioBlockDeviceConfiguration(attachment: attachment)
            disk.blockDeviceIdentifier = identifier
            return disk
        }
        let network = VZVirtioNetworkDeviceConfiguration()
        network.attachment = VZNATNetworkDeviceAttachment()
        network.macAddress = VZMACAddress(string: "02:72:32:61:75:01")!
        configuration.networkDevices = [network]
        configuration.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
        configuration.socketDevices = [VZVirtioSocketDeviceConfiguration()]
        let serial = VZVirtioConsoleDeviceSerialPortConfiguration()
        let log = owned("guest-serial.log")
        if !FileManager.default.fileExists(atPath: log.path) { FileManager.default.createFile(atPath: log.path, contents: nil) }
        let output = try FileHandle(forWritingTo: log)
        try output.seekToEnd()
        let consolePipe = Pipe()
        consoleInput = consolePipe.fileHandleForWriting
        serial.attachment = VZFileHandleSerialPortAttachment(fileHandleForReading: consolePipe.fileHandleForReading, fileHandleForWriting: output)
        configuration.serialPorts = [serial]
        try configuration.validate()
        return configuration
    }

    func status() -> [String: Any] {
        ["owner": owner, "pid": ProcessInfo.processInfo.processIdentifier, "state": machine.map { String(describing: $0.state) } ?? "absent", "state_raw": machine?.state.rawValue ?? -1, "changing_state": changingState, "ssh_host": "127.0.0.1", "ssh_port": sshPort, "vsock_port": 2222, "vm_dir": root.path, "os_cache": osCachingMode == .cached ? "cached" : "uncached", "audit_cache": "uncached", "disk_synchronization": "full"]
    }

    func start(_ completed: @escaping ([String: Any]) -> Void) {
        guard !changingState, machine == nil || machine?.state == .stopped || machine?.state == .error else {
            completed(["ok": false, "error": "VM is already active or transitioning"]); return
        }
        do {
            let vm = VZVirtualMachine(configuration: try configuration())
            vm.delegate = self
            machine = vm
            changingState = true
            vm.start { result in
                self.changingState = false
                switch result {
                case .success: event("started", self.status()); completed(["ok": true, "status": self.status()])
                case .failure(let error): event("start_failed", ["error": String(describing: error)]); completed(["ok": false, "error": String(describing: error)])
                }
            }
        } catch { completed(["ok": false, "error": String(describing: error)]); event("configuration_failed", ["error": String(describing: error)]) }
    }

    func stop(_ completed: @escaping ([String: Any]) -> Void) {
        guard !changingState, let vm = machine, vm.canStop else {
            completed(["ok": false, "error": "VM cannot be stopped in current state", "status": status()]); return
        }
        changingState = true
        event("forced_stop_requested", status())
        vm.stop { error in
            self.changingState = false
            if let error { completed(["ok": false, "error": String(describing: error)]); return }
            event("forced_stopped", self.status())
            completed(["ok": true, "status": self.status()])
        }
    }

    func command(_ line: String, fd: Int32) {
        guard let data = line.data(using: .utf8), let input = try? JSONSerialization.jsonObject(with: data) as? [String: Any], let op = input["command"] as? String else {
            reply(fd, ["ok": false, "error": "Expected JSON command"]); return
        }
        switch op {
        case "status": reply(fd, ["ok": true, "status": status()])
        case "start": start { reply(fd, $0) }
        case "stop": stop { reply(fd, $0) }
        case "console-input":
            guard machine?.state == .running, !changingState, let input = input["text"] as? String, let data = input.data(using: .utf8), data.count <= 2048, let console = consoleInput else {
                reply(fd, ["ok": false, "error": "Expected bounded console text for an active VM"]); return
            }
            do { try console.write(contentsOf: data); reply(fd, ["ok": true, "bytes": data.count]) }
            catch { reply(fd, ["ok": false, "error": String(describing: error)]) }
        case "request-stop":
            guard let vm = machine, vm.canRequestStop else { reply(fd, ["ok": false, "error": "VM cannot accept a stop request"]); return }
            do { try vm.requestStop(); reply(fd, ["ok": true, "requested": true]) }
            catch { reply(fd, ["ok": false, "error": String(describing: error)]) }
        case "shutdown":
            if machine?.canStop == true {
                stop { result in reply(fd, result); if result["ok"] as? Bool == true { exit(0) } }
            } else if !changingState { reply(fd, ["ok": true]); exit(0) }
            else { reply(fd, ["ok": false, "error": "VM transition in progress"]) }
        default: reply(fd, ["ok": false, "error": "Unknown command"])
        }
    }

    func controlListener() {
        let path = owned("control.sock").path
        guard path.utf8.count < 104 else { fail("Owned control path exceeds Unix socket limit") }
        if FileManager.default.fileExists(atPath: path) { fail("Owned control socket already exists; inspect previous runner before removing it") }
        let fd = makeSocket(AF_UNIX)
        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutableBytes(of: &address.sun_path) { target in
            target.copyBytes(from: Array(path.utf8) + [0])
        }
        let bound = withUnsafePointer(to: &address) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size)) } }
        guard bound == 0, listen(fd, 8) == 0 else { fail("Cannot bind owned control socket: \(errno)") }
        chmod(path, 0o600)
        DispatchQueue.global().async {
            while true {
                let client = accept(fd, nil, nil)
                if client < 0 { if errno == EINTR { continue }; return }
                DispatchQueue.global().async {
                    var input = Data(); var byte: UInt8 = 0
                    while input.count < 4096 && Darwin.read(client, &byte, 1) == 1 { if byte == 10 { break }; input.append(byte) }
                    let line = String(decoding: input, as: UTF8.self)
                    DispatchQueue.main.async { self.command(line, fd: client) }
                }
            }
        }
    }

    func sshListener() {
        let fd = makeSocket(AF_INET)
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_addr.s_addr = inet_addr("127.0.0.1")
        address.sin_port = 0
        let bound = withUnsafePointer(to: &address) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { Darwin.bind(fd, $0, socklen_t(MemoryLayout<sockaddr_in>.size)) } }
        guard bound == 0, listen(fd, 8) == 0 else { fail("Cannot bind loopback SSH bridge: \(errno)") }
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        _ = withUnsafeMutablePointer(to: &address) { $0.withMemoryRebound(to: sockaddr.self, capacity: 1) { getsockname(fd, $0, &length) } }
        sshPort = UInt16(bigEndian: address.sin_port)
        DispatchQueue.global().async {
            while true {
                let client = accept(fd, nil, nil)
                if client < 0 { if errno == EINTR { continue }; return }
                DispatchQueue.main.async { self.bridge(client) }
            }
        }
    }

    func bridge(_ client: Int32) {
        guard let vm = machine, vm.state == .running, let socket = vm.socketDevices.first as? VZVirtioSocketDevice else { Darwin.close(client); return }
        socket.connect(toPort: 2222) { result in
            guard case .success(let connection) = result else { Darwin.close(client); return }
            let id = UUID(); self.bridges[id] = connection
            let guest = connection.fileDescriptor
            let group = DispatchGroup()
            for (source, target) in [(client, guest), (guest, client)] {
                group.enter()
                DispatchQueue.global().async {
                    var bytes = [UInt8](repeating: 0, count: 65536)
                    while true {
                        let count = Darwin.read(source, &bytes, bytes.count)
                        if count < 0 && errno == EINTR { continue }
                        if count <= 0 { break }
                        let sent = bytes.withUnsafeBytes { sendAll(target, $0.baseAddress!, count) }
                        if !sent { break }
                    }
                    Darwin.shutdown(target, SHUT_WR)
                    group.leave()
                }
            }
            group.notify(queue: .main) { connection.close(); self.bridges.removeValue(forKey: id); Darwin.close(client) }
        }
    }

    func guestDidStop(_ virtualMachine: VZVirtualMachine) { event("guest_stopped", status()) }
    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) { event("vm_error", ["error": String(describing: error)]) }
}

var options: [String: String] = [:]
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let key = args.removeFirst()
    guard ["--vm-dir", "--cpus", "--memory-gib", "--os-cache"].contains(key), !args.isEmpty else { fail("Use --vm-dir <owned directory> [--cpus 2..4] [--memory-gib 2..8] [--os-cache cached|uncached]") }
    options[key] = args.removeFirst()
}
guard let rootPath = options["--vm-dir"], let cpus = Int(options["--cpus"] ?? "2"), let memory = Int(options["--memory-gib"] ?? "4"), (2...4).contains(cpus), (2...8).contains(memory) else { fail("Invalid bounded VM configuration") }
let osCache = options["--os-cache"] ?? "uncached"
guard ["cached", "uncached"].contains(osCache) else { fail("Invalid OS disk caching mode") }
guard VZVirtualMachine.isSupported else { fail("Virtualization.framework is unsupported") }
signal(SIGPIPE, SIG_IGN)
do {
    let runner = try Runner(root: URL(fileURLWithPath: rootPath, isDirectory: true), cpuCount: cpus, memoryGiB: memory, osCachingMode: osCache == "cached" ? .cached : .uncached)
    runner.controlListener()
    runner.sshListener()
    event("runner_ready", runner.status())
    runner.start { result in if result["ok"] as? Bool != true { event("startup_error", result) } }
    withExtendedLifetime(runner) { dispatchMain() }
} catch { fail("Cannot initialize disposable VM: \(error)") }
