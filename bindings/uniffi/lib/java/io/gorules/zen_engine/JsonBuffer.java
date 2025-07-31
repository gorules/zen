package io.gorules.zen_engine;

import org.jetbrains.annotations.NotNull;

import java.nio.charset.StandardCharsets;

public record JsonBuffer(byte[] value) {
    public JsonBuffer(String value) {
        this(value.getBytes(StandardCharsets.UTF_8));
    }

    @NotNull
    @Override
    public String toString() {
        return new String(value, StandardCharsets.UTF_8);
    }
}