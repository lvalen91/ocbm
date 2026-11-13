package zeno.gmccpa.ocbm

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test

/** Plan v5.7 U8 / audit12 F119: the phone's user-given name never reaches a log line. */
class PhoneIdentRedactTest {

    @Test fun nameReplacedOtherFieldsKept() {
        val j = "{\"name\":\"Alice's iPhone\",\"deviceID\":\"AA:BB:CC:DD:EE:FF\",\"model\":\"iPhone15,2\",\"osName\":\"iOS\",\"osVersion\":\"18.0\"}"
        val r = PhoneIdent.redactName(j)
        assertEquals("{\"name\":\"<name 14 chars>\",\"deviceID\":\"AA:BB:CC:DD:EE:FF\",\"model\":\"iPhone15,2\",\"osName\":\"iOS\",\"osVersion\":\"18.0\"}", r)
        assertFalse(r.contains("Alice"))
    }

    @Test fun escapesAreHonouredAndCountedDecoded() {
        // A "q" é\b -> 9 characters once decoded; the escaped quotes must not end the match early.
        val j = "{\"name\":\"A \\\"q\\\" \\u00e9\\\\b\",\"model\":\"x\"}"
        val r = PhoneIdent.redactName(j)
        assertEquals("{\"name\":\"<name 9 chars>\",\"model\":\"x\"}", r)
        assertFalse(r.contains("q"))
    }

    @Test fun surrogatePairsCountAsOneCharacter() {
        assertEquals("{\"name\":\"<name 2 chars>\"}", PhoneIdent.redactName("{\"name\":\"B\\ud83d\\ude00\"}"))
        assertEquals("{\"name\":\"<name 2 chars>\"}", PhoneIdent.redactName("{\"name\":\"B😀\"}"))
    }

    @Test fun whitespaceAndEmptyValue() {
        assertEquals("{ \"name\" : \"<name 3 chars>\" }", PhoneIdent.redactName("{ \"name\" : \"Bob\" }"))
        assertEquals("{\"name\":\"<name 0 chars>\"}", PhoneIdent.redactName("{\"name\":\"\"}"))
    }

    @Test fun onlyTheNameKeyIsTouched() {
        val j = "{\"nickname\":\"Bob\",\"model\":\"name\",\"x\":\"\\\"name\\\":\\\"y\\\"\"}"
        assertEquals(j, PhoneIdent.redactName(j))
    }

    @Test fun unclosedNameValueIsCutNotLeaked() {
        val r = PhoneIdent.redactName("{\"model\":\"x\",\"name\":\"Bob Smi")
        assertEquals("{\"model\":\"x\",\"name\":\"<name truncated>", r)
        assertFalse(r.contains("Bob"))
    }

    @Test fun longNameDoesNotOverflowTheRegex() {
        val long = "n".repeat(200_000)
        assertEquals("{\"name\":\"<name 200000 chars>\"}", PhoneIdent.redactName("{\"name\":\"$long\"}"))
    }

    @Test fun placeholderCountsCodePoints() {
        assertEquals("<name 2 chars>", PhoneIdent.placeholder("B😀"))
    }
}
