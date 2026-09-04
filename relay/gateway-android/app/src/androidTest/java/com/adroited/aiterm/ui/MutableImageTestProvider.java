package com.adroited.aiterm.ui;

import android.content.ContentProvider;
import android.content.ContentValues;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.os.ParcelFileDescriptor;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.Objects;
import java.util.UUID;

/** Test-only source that can serve different bytes each time it is opened. */
public final class MutableImageTestProvider extends ContentProvider {
    private File first;
    private File later;
    private int opens;
    private int slowBytes;
    private int slowChunkBytes;
    private long slowDelayMillis;
    private long slowStartDelayMillis;
    private long streamedBytes;

    @Override
    public boolean onCreate() {
        return true;
    }

    @Override
    public synchronized Bundle call(String method, String arg, Bundle extras) {
        switch (method) {
            case "configure": {
                reset();
                Bundle source = Objects.requireNonNull(extras);
                first = writeBytes(Objects.requireNonNull(source.getByteArray("first")));
                later = writeBytes(Objects.requireNonNull(source.getByteArray("later")));
                return Bundle.EMPTY;
            }
            case "configure-generated": {
                reset();
                int length = Objects.requireNonNull(extras).getInt("length");
                first = writeGenerated(length);
                later = first;
                return Bundle.EMPTY;
            }
            case "configure-slow": {
                reset();
                Bundle source = Objects.requireNonNull(extras);
                slowBytes = source.getInt("length");
                slowChunkBytes = source.getInt("chunk");
                slowDelayMillis = source.getLong("delay");
                slowStartDelayMillis = source.getLong("start-delay");
                return Bundle.EMPTY;
            }
            case "stats": {
                Bundle result = new Bundle();
                result.putInt("opens", opens);
                result.putLong("streamed", streamedBytes);
                return result;
            }
            case "reset":
                reset();
                return Bundle.EMPTY;
            default:
                return super.call(method, arg, extras);
        }
    }

    @Override
    public synchronized ParcelFileDescriptor openFile(Uri uri, String mode) {
        if (!"r".equals(mode)) throw new IllegalArgumentException("read-only test provider");
        opens += 1;
        try {
            if (slowBytes > 0) return openSlowPipe();
            File source = opens == 1 ? first : later;
            if (source == null) throw new IllegalStateException("test source is not configured");
            return ParcelFileDescriptor.open(source, ParcelFileDescriptor.MODE_READ_ONLY);
        } catch (IOException error) {
            throw new IllegalStateException(error);
        }
    }

    @Override public String getType(Uri uri) { return "image/jpeg"; }
    @Override public Cursor query(Uri uri, String[] projection, String selection, String[] selectionArgs, String sortOrder) { return null; }
    @Override public Uri insert(Uri uri, ContentValues values) { return null; }
    @Override public int delete(Uri uri, String selection, String[] selectionArgs) { return 0; }
    @Override public int update(Uri uri, ContentValues values, String selection, String[] selectionArgs) { return 0; }

    private ParcelFileDescriptor openSlowPipe() throws IOException {
        ParcelFileDescriptor[] pipe = ParcelFileDescriptor.createPipe();
        final ParcelFileDescriptor write = pipe[1];
        final int total = slowBytes;
        final byte[] chunk = new byte[slowChunkBytes];
        final long delay = slowDelayMillis;
        final long startDelay = slowStartDelayMillis;
        new Thread(() -> {
            try (ParcelFileDescriptor.AutoCloseOutputStream output =
                    new ParcelFileDescriptor.AutoCloseOutputStream(write)) {
                Thread.sleep(startDelay);
                int remaining = total;
                while (remaining > 0) {
                    int written = Math.min(remaining, chunk.length);
                    output.write(chunk, 0, written);
                    output.flush();
                    synchronized (MutableImageTestProvider.this) { streamedBytes += written; }
                    remaining -= written;
                    Thread.sleep(delay);
                }
            } catch (IOException ignored) {
                // The consumer may cancel midway through a test.
            } catch (InterruptedException interrupted) {
                Thread.currentThread().interrupt();
            }
        }, "mutable-image-test-provider").start();
        return pipe[0];
    }

    private File writeBytes(byte[] bytes) {
        File file = new File(Objects.requireNonNull(getContext()).getCacheDir(), "mutable-image-" + UUID.randomUUID());
        try (FileOutputStream output = new FileOutputStream(file)) {
            output.write(bytes);
            return file;
        } catch (IOException error) {
            throw new IllegalStateException(error);
        }
    }

    private File writeGenerated(int length) {
        File file = new File(Objects.requireNonNull(getContext()).getCacheDir(), "mutable-image-" + UUID.randomUUID());
        try (FileOutputStream output = new FileOutputStream(file)) {
            byte[] chunk = new byte[32 * 1024];
            int remaining = length;
            while (remaining > 0) {
                int written = Math.min(remaining, chunk.length);
                output.write(chunk, 0, written);
                remaining -= written;
            }
            return file;
        } catch (IOException error) {
            throw new IllegalStateException(error);
        }
    }

    private void reset() {
        if (first != null) first.delete();
        if (later != null && later != first) later.delete();
        first = null;
        later = null;
        opens = 0;
        slowBytes = 0;
        slowChunkBytes = 0;
        slowDelayMillis = 0;
        slowStartDelayMillis = 0;
        streamedBytes = 0;
    }
}
