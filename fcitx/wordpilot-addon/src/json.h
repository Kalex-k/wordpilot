// A minimal JSON value and parser, scoped to exactly what the WordPilot wire
// protocol needs (see wordpilot-protocol/src/message.rs for the Rust side of
// this contract). This is not a general-purpose JSON library: no comments,
// no streaming, no error recovery beyond "parsing failed". Pulling in a full
// JSON dependency for a thin adapter that only ever talks to one well-tested
// peer would be more code than this.
#pragma once

#include <cstdint>
#include <map>
#include <string>
#include <vector>

namespace wordpilot {

enum class JsonType { Null, Boolean, Number, String, Array, Object };

class JsonValue {
public:
    JsonValue() : type_(JsonType::Null) {}

    static JsonValue parse(const std::string &text);

    // Public constructors used by the parser. Kept as named static functions
    // rather than a `friend class JsonParser` so the parser stays an
    // ordinary consumer of this type instead of reaching into its privates --
    // one less thing to keep in sync if the parser implementation moves.
    static JsonValue makeNull() { return JsonValue(); }
    static JsonValue makeBoolean(bool value) {
        JsonValue v;
        v.type_ = JsonType::Boolean;
        v.boolean_ = value;
        return v;
    }
    static JsonValue makeNumber(double value) {
        JsonValue v;
        v.type_ = JsonType::Number;
        v.number_ = value;
        return v;
    }
    static JsonValue makeString(std::string value) {
        JsonValue v;
        v.type_ = JsonType::String;
        v.string_ = std::move(value);
        return v;
    }
    static JsonValue makeArray(std::vector<JsonValue> items) {
        JsonValue v;
        v.type_ = JsonType::Array;
        v.array_ = std::move(items);
        return v;
    }
    static JsonValue makeObject(std::map<std::string, JsonValue> members) {
        JsonValue v;
        v.type_ = JsonType::Object;
        v.object_ = std::move(members);
        return v;
    }

    JsonType type() const { return type_; }
    bool isNull() const { return type_ == JsonType::Null; }
    bool isObject() const { return type_ == JsonType::Object; }
    bool isArray() const { return type_ == JsonType::Array; }
    bool isString() const { return type_ == JsonType::String; }

    // Object member access. Returns a null JsonValue (not a reference into a
    // temporary) when the key is absent, so chained calls like
    // `value.get("a").get("b").asString()` never dereference a dangling
    // pointer.
    const JsonValue &get(const std::string &key) const;

    const std::vector<JsonValue> &items() const { return array_; }

    // By value: returning a reference would dangle whenever the fallback is
    // the default-argument temporary and the result outlives the call.
    std::string asString(const std::string &fallback = "") const {
        return type_ == JsonType::String ? string_ : fallback;
    }
    double asNumber(double fallback = 0.0) const {
        return type_ == JsonType::Number ? number_ : fallback;
    }
    bool asBool(bool fallback = false) const {
        return type_ == JsonType::Boolean ? boolean_ : fallback;
    }

    // Builds a JSON string literal, including the surrounding quotes, with
    // control characters, quotes and backslashes escaped. Used to build
    // requests, since everything the daemon receives from us is simple
    // key/value pairs -- a full serializer is unneeded.
    static std::string escapeString(const std::string &raw);

private:
    JsonType type_;
    bool boolean_ = false;
    double number_ = 0.0;
    std::string string_;
    std::vector<JsonValue> array_;
    std::map<std::string, JsonValue> object_;
};

} // namespace wordpilot
