#include "json.h"

#include <cctype>
#include <cstdio>
#include <stdexcept>

namespace wordpilot {

namespace {

class JsonParser {
public:
    explicit JsonParser(const std::string &text) : text_(text) {}

    JsonValue parseValue() {
        skipWhitespace();
        if (pos_ >= text_.size()) {
            throw std::runtime_error("unexpected end of JSON input");
        }
        switch (text_[pos_]) {
        case '{':
            return parseObject();
        case '[':
            return parseArray();
        case '"':
            return parseString();
        case 't':
        case 'f':
            return parseBool();
        case 'n':
            return parseNull();
        default:
            return parseNumber();
        }
    }

private:
    const std::string &text_;
    size_t pos_ = 0;

    void skipWhitespace() {
        while (pos_ < text_.size() &&
               std::isspace(static_cast<unsigned char>(text_[pos_]))) {
            ++pos_;
        }
    }

    char peek() const { return pos_ < text_.size() ? text_[pos_] : '\0'; }

    void expect(char c) {
        if (pos_ >= text_.size() || text_[pos_] != c) {
            throw std::runtime_error(std::string("expected '") + c + "'");
        }
        ++pos_;
    }

    JsonValue parseObject() {
        std::map<std::string, JsonValue> members;
        expect('{');
        skipWhitespace();
        if (peek() == '}') {
            ++pos_;
            return JsonValue::makeObject(std::move(members));
        }
        while (true) {
            skipWhitespace();
            std::string key = parseStringLiteral();
            skipWhitespace();
            expect(':');
            members[key] = parseValue();
            skipWhitespace();
            if (peek() == ',') {
                ++pos_;
                continue;
            }
            break;
        }
        skipWhitespace();
        expect('}');
        return JsonValue::makeObject(std::move(members));
    }

    JsonValue parseArray() {
        std::vector<JsonValue> items;
        expect('[');
        skipWhitespace();
        if (peek() == ']') {
            ++pos_;
            return JsonValue::makeArray(std::move(items));
        }
        while (true) {
            items.push_back(parseValue());
            skipWhitespace();
            if (peek() == ',') {
                ++pos_;
                continue;
            }
            break;
        }
        skipWhitespace();
        expect(']');
        return JsonValue::makeArray(std::move(items));
    }

    JsonValue parseString() { return JsonValue::makeString(parseStringLiteral()); }

    // Parses a quoted JSON string and returns its decoded contents, without
    // wrapping it in a JsonValue -- object keys need the raw string, not a
    // JsonValue.
    std::string parseStringLiteral() {
        expect('"');
        std::string out;
        while (pos_ < text_.size() && text_[pos_] != '"') {
            char c = text_[pos_++];
            if (c == '\\' && pos_ < text_.size()) {
                char escaped = text_[pos_++];
                switch (escaped) {
                case '"':
                    out.push_back('"');
                    break;
                case '\\':
                    out.push_back('\\');
                    break;
                case '/':
                    out.push_back('/');
                    break;
                case 'n':
                    out.push_back('\n');
                    break;
                case 't':
                    out.push_back('\t');
                    break;
                case 'r':
                    out.push_back('\r');
                    break;
                case 'b':
                    out.push_back('\b');
                    break;
                case 'f':
                    out.push_back('\f');
                    break;
                case 'u':
                    // \uXXXX: the daemon never sends non-BMP escapes or
                    // surrogate pairs in the fields this adapter reads
                    // (candidate text, error messages), so only the common
                    // BMP case is decoded. A malformed escape degrades to a
                    // literal '?' rather than corrupting the rest of the
                    // parse.
                    if (pos_ + 4 <= text_.size()) {
                        unsigned codepoint = 0;
                        for (int i = 0; i < 4; ++i) {
                            char hex = text_[pos_++];
                            codepoint <<= 4;
                            if (hex >= '0' && hex <= '9') {
                                codepoint |= static_cast<unsigned>(hex - '0');
                            } else if (hex >= 'a' && hex <= 'f') {
                                codepoint |= static_cast<unsigned>(hex - 'a' + 10);
                            } else if (hex >= 'A' && hex <= 'F') {
                                codepoint |= static_cast<unsigned>(hex - 'A' + 10);
                            }
                        }
                        appendUtf8(out, codepoint);
                    }
                    break;
                default:
                    out.push_back(escaped);
                }
            } else {
                out.push_back(c);
            }
        }
        expect('"');
        return out;
    }

    static void appendUtf8(std::string &out, unsigned codepoint) {
        if (codepoint <= 0x7F) {
            out.push_back(static_cast<char>(codepoint));
        } else if (codepoint <= 0x7FF) {
            out.push_back(static_cast<char>(0xC0 | (codepoint >> 6)));
            out.push_back(static_cast<char>(0x80 | (codepoint & 0x3F)));
        } else {
            out.push_back(static_cast<char>(0xE0 | (codepoint >> 12)));
            out.push_back(static_cast<char>(0x80 | ((codepoint >> 6) & 0x3F)));
            out.push_back(static_cast<char>(0x80 | (codepoint & 0x3F)));
        }
    }

    JsonValue parseBool() {
        if (text_.compare(pos_, 4, "true") == 0) {
            pos_ += 4;
            return JsonValue::makeBoolean(true);
        }
        if (text_.compare(pos_, 5, "false") == 0) {
            pos_ += 5;
            return JsonValue::makeBoolean(false);
        }
        throw std::runtime_error("invalid literal where a bool was expected");
    }

    JsonValue parseNull() {
        if (text_.compare(pos_, 4, "null") != 0) {
            throw std::runtime_error("invalid literal where null was expected");
        }
        pos_ += 4;
        return JsonValue::makeNull();
    }

    JsonValue parseNumber() {
        size_t start = pos_;
        if (peek() == '-') {
            ++pos_;
        }
        while (pos_ < text_.size() &&
               (std::isdigit(static_cast<unsigned char>(text_[pos_])) ||
                text_[pos_] == '.' || text_[pos_] == 'e' || text_[pos_] == 'E' ||
                text_[pos_] == '+' || text_[pos_] == '-')) {
            ++pos_;
        }
        if (pos_ == start) {
            throw std::runtime_error("expected a number");
        }
        return JsonValue::makeNumber(std::stod(text_.substr(start, pos_ - start)));
    }
};

} // namespace

JsonValue JsonValue::parse(const std::string &text) {
    try {
        JsonParser parser(text);
        return parser.parseValue();
    } catch (const std::exception &) {
        // Malformed input from the daemon is always recoverable at the call
        // site (same spirit as ProtocolError::is_recoverable on the Rust
        // side): return Null rather than propagating a C++ exception across
        // the Fcitx key-event boundary.
        return JsonValue();
    }
}

const JsonValue &JsonValue::get(const std::string &key) const {
    static const JsonValue kNull;
    if (type_ != JsonType::Object) {
        return kNull;
    }
    auto it = object_.find(key);
    return it == object_.end() ? kNull : it->second;
}

std::string JsonValue::escapeString(const std::string &raw) {
    std::string out;
    out.reserve(raw.size() + 2);
    out.push_back('"');
    for (unsigned char c : raw) {
        switch (c) {
        case '"':
            out += "\\\"";
            break;
        case '\\':
            out += "\\\\";
            break;
        case '\n':
            out += "\\n";
            break;
        case '\t':
            out += "\\t";
            break;
        case '\r':
            out += "\\r";
            break;
        default:
            if (c < 0x20) {
                char buf[8];
                std::snprintf(buf, sizeof(buf), "\\u%04x", c);
                out += buf;
            } else {
                // Bytes >= 0x80 are passed through unchanged: JSON strings
                // are UTF-8, and the daemon's context text (umlauts, etc.)
                // needs no escaping, only control characters and the two
                // JSON metacharacters do.
                out.push_back(static_cast<char>(c));
            }
        }
    }
    out.push_back('"');
    return out;
}

} // namespace wordpilot
