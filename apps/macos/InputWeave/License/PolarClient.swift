import Foundation

/// Polar's public license-key endpoints, as docs/architecture.md §11
/// describes them: requests are built and answers read here, the bytes
/// go through a `PolarTransport`, so tests read the shared fixtures in
/// testdata/polar without a network.
struct PolarClient: Sendable {
    struct Config: Sendable, Equatable {
        var server: URL
        var organizationID: String
        /// Sent as `User-Agent: InputWeave/<version>`: Cloudflare, in front
        /// of Polar, refuses some default user agents.
        var version: String

        /// A build without Polar's values: every request fails, so nothing
        /// is ever activated.
        static let unconfigured = Config(server: URL(string: "https://polar.invalid")!, organizationID: "", version: "0")

        /// From the app's Info.plist, which project.yml writes: the one
        /// place the server and the organization are named. A Debug build
        /// also takes `-polarServer <url>` and `-polarOrganization <id>` on
        /// the command line, to talk to scripts/mock-polar.py. Nil when the
        /// build has neither.
        static func fromBundle(_ bundle: Bundle = .main, defaults: UserDefaults = .standard) -> Config? {
            var info = bundle.infoDictionary ?? [:]
            #if DEBUG
            if let given = defaults.string(forKey: "polarServer") { info["InputWeavePolarServer"] = given }
            if let given = defaults.string(forKey: "polarOrganization") { info["InputWeavePolarOrganizationID"] = given }
            #endif
            return fromInfo(info, version: bundle.shortVersion)
        }

        /// From an Info.plist's values.
        static func fromInfo(_ info: [String: Any], version: String) -> Config? {
            guard let server = (info["InputWeavePolarServer"] as? String).flatMap(URL.init(string:)),
                let organization = info["InputWeavePolarOrganizationID"] as? String, !organization.isEmpty else {
                return nil
            }
            return Config(server: server, organizationID: organization, version: version)
        }
    }

    /// Polar's answer to `activate`.
    enum Activation: Equatable, Sendable {
        /// 200: the new activation, the buyer's email, and when Polar said so.
        case activated(id: String, email: String, at: Date?)
        /// 200 without an activation id that can be read: Polar took a seat
        /// that cannot be reported or freed.
        case activatedUnnamed
        /// 403 NotPermitted: every seat is taken, or the key is no longer
        /// active; `validate` without an activation id tells which.
        case notPermitted
        /// 404, or a 422 about the key: no such key, or one Polar could not
        /// read.
        case invalid
        /// A 422 about anything else: this request is at fault, not the
        /// key, so it is no refusal.
        case rejected
        case rateLimited(retryAfter: Duration)
        /// No answer that counts: no network, Cloudflare's own 403, a 5xx.
        case unreachable
    }

    /// Polar's answer to `validate`.
    enum Validation: Equatable, Sendable {
        case valid(email: String, at: Date?)
        /// 404, or a 422 about the key or the activation id: the key, or
        /// this computer's activation, is gone.
        case notFound
        case rateLimited(retryAfter: Duration)
        case unreachable
    }

    /// Polar's answer to `deactivate`.
    enum Deactivation: Equatable, Sendable {
        /// 204, or 404 because the activation is already gone.
        case done
        /// 403 NotPermitted, or a 422 about the key or the activation id:
        /// Polar will never take it; nothing to retry.
        case refused
        case rateLimited(retryAfter: Duration)
        case unreachable
    }

    let config: Config
    let transport: any PolarTransport

    /// How long a request waits for Polar's answer,
    static let timeout: TimeInterval = 20
    /// and an activation longer: an answer lost to a timeout may still have
    /// taken a seat, which asking again takes twice.
    static let activateTimeout: TimeInterval = 60
    /// The longest label sent.
    static let labelLength = 64

    init(config: Config, transport: any PolarTransport = URLSessionPolarTransport()) {
        self.config = config
        self.transport = transport
    }

    func activate(key: String, label: String) async -> Activation {
        guard let answer = await post(
            "activate", ["key": key, "organization_id": config.organizationID, "label": Self.label(label)],
            timeout: Self.activateTimeout) else {
            return .unreachable
        }
        switch answer.status {
        case 200:
            let body = answer.json
            guard let id = body?["id"] as? String, !id.isEmpty else { return .activatedUnnamed }
            let customer = (body?["license_key"] as? [String: Any])?["customer"] as? [String: Any]
            return .activated(id: id, email: customer?["email"] as? String ?? "", at: answer.date)
        case 403:
            return answer.isPolarRefusal ? .notPermitted : .unreachable
        case 404:
            return .invalid
        case 422:
            return answer.concerns(["key"]) ? .invalid : .rejected
        case 429:
            return .rateLimited(retryAfter: answer.retryAfter)
        default:
            return .unreachable
        }
    }

    /// Validates this computer's activation, or, with no activation id, the
    /// key alone (to tell seats full from a dead key after a 403).
    func validate(key: String, activationID: String?) async -> Validation {
        var body = ["key": key, "organization_id": config.organizationID]
        if let activationID { body["activation_id"] = activationID }
        guard let answer = await post("validate", body) else { return .unreachable }
        switch answer.status {
        case 200:
            let customer = answer.json?["customer"] as? [String: Any]
            return .valid(email: customer?["email"] as? String ?? "", at: answer.date)
        case 404:
            return .notFound
        case 422:
            return answer.concerns(["key", "activation_id"]) ? .notFound : .unreachable
        case 429:
            return .rateLimited(retryAfter: answer.retryAfter)
        default:
            return .unreachable
        }
    }

    func deactivate(key: String, activationID: String) async -> Deactivation {
        guard let answer = await post("deactivate", [
            "key": key, "organization_id": config.organizationID, "activation_id": activationID,
        ]) else { return .unreachable }
        switch answer.status {
        case 200, 204, 404: return .done
        case 422: return answer.concerns(["key", "activation_id"]) ? .refused : .unreachable
        case 403: return answer.isPolarRefusal ? .refused : .unreachable
        case 429: return .rateLimited(retryAfter: answer.retryAfter)
        default: return .unreachable
        }
    }

    /// This computer's name as Polar shows it: on one line, at most
    /// `labelLength` characters, and "Mac" when nothing is left.
    static func label(_ name: String) -> String {
        let words = name.components(separatedBy: .whitespacesAndNewlines.union(.controlCharacters)).filter { !$0.isEmpty }
        let label = String(words.joined(separator: " ").prefix(labelLength)).trimmingCharacters(in: .whitespaces)
        return label.isEmpty ? "Mac" : label
    }

    /// The request for one endpoint; public for the tests.
    func request(_ endpoint: String, _ body: [String: String], timeout: TimeInterval = Self.timeout) -> URLRequest {
        var request = URLRequest(url: config.server.appending(path: "v1/customer-portal/license-keys/\(endpoint)"))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue("InputWeave/\(config.version)", forHTTPHeaderField: "User-Agent")
        request.httpBody = try? JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])
        request.timeoutInterval = timeout
        return request
    }

    private func post(_ endpoint: String, _ body: [String: String], timeout: TimeInterval = Self.timeout) async -> PolarAnswer? {
        try? await transport.send(request(endpoint, body, timeout: timeout))
    }
}

/// What came back from Polar: the status, the body and two headers.
struct PolarAnswer: Sendable {
    var status: Int
    var body: Data
    var dateHeader: String?
    var retryAfterHeader: String?

    var json: [String: Any]? {
        (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
    }

    /// Only a 403 whose body is Polar's own error counts as a refusal;
    /// Cloudflare's 403 has no `error` field.
    var isPolarRefusal: Bool {
        json?["error"] as? String == "NotPermitted"
    }

    /// A 422 names what Polar could not read: each entry of `detail` has
    /// its place in the request in `loc`, such as `["body", "key"]`.
    func concerns(_ fields: Set<String>) -> Bool {
        guard let detail = json?["detail"] as? [[String: Any]] else { return false }
        return detail.contains { entry in
            (entry["loc"] as? [Any] ?? []).contains { ($0 as? String).map(fields.contains) ?? false }
        }
    }

    var date: Date? {
        dateHeader.flatMap(Self.httpDate.date(from:))
    }

    /// HTTP's date format; DateFormatter parses safely from any thread.
    private nonisolated(unsafe) static let httpDate: DateFormatter = {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = TimeZone(identifier: "GMT")
        formatter.dateFormat = "EEE, dd MMM yyyy HH:mm:ss zzz"
        return formatter
    }()

    /// `Retry-After` in seconds, a minute when missing, at most an hour.
    var retryAfter: Duration {
        let seconds = retryAfterHeader.flatMap { Int($0.trimmingCharacters(in: .whitespaces)) } ?? 60
        return .seconds(min(max(seconds, 1), 3600))
    }
}

protocol PolarTransport: Sendable {
    /// Sends the request; throws when no answer came.
    func send(_ request: URLRequest) async throws -> PolarAnswer
}

struct URLSessionPolarTransport: PolarTransport {
    func send(_ request: URLRequest) async throws -> PolarAnswer {
        let (data, response) = try await URLSession.shared.data(for: request)
        guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
        return PolarAnswer(
            status: http.statusCode, body: data,
            dateHeader: http.value(forHTTPHeaderField: "Date"),
            retryAfterHeader: http.value(forHTTPHeaderField: "Retry-After"))
    }
}
