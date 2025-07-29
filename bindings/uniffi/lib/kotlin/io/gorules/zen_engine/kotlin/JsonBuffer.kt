package io.gorules.zen_engine.kotlin

@JvmInline
value class JsonBuffer(val value: ByteArray) {

    constructor(value: String) : this(value = value.toByteArray(Charsets.UTF_8))

    override fun toString(): String {
        return value.toString(Charsets.UTF_8)
    }
}