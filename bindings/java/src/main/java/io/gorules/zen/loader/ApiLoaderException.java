package io.gorules.zen.loader;

/**
 * Exception thrown when ApiDecisionLoader fails to load a decision from the API.
 */
public class ApiLoaderException extends RuntimeException {

    /**
     * Create exception with message.
     *
     * @param message Error message
     */
    public ApiLoaderException(String message) {
        super(message);
    }

    /**
     * Create exception with message and cause.
     *
     * @param message Error message
     * @param cause   Underlying cause
     */
    public ApiLoaderException(String message, Throwable cause) {
        super(message, cause);
    }
}
