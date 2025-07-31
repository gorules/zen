package io.gorules.zen_engine.kotlin

@JvmInline
value class JsonBuffer(val value: ByteArray) {
    constructor(json: String) : this(json.toByteArray(Charsets.UTF_8))

    override fun toString(): String = value.toString(Charsets.UTF_8)
    fun toByteArray(): ByteArray = value
}