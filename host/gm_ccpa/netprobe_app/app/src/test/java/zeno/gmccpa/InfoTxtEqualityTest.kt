package zeno.gmccpa

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Test
import zeno.gmccpa.av.BPlist

/**
 * docs/11 item 5.12: the Bonjour TXT record and the served `/info` must agree on the identity
 * (`deviceid`/`features`/`pi`) or pair-verify fails on the unit with `-16720` — a desync that
 * only ever showed on hardware. The TXT comes from `CarPlayRx.txtRecordFor` (the map both adverts
 * publish); `/info` is the static `assets/info.bplist`, decoded with the app's own `BPlist`.
 */
class InfoTxtEqualityTest {
    private fun infoPlist(): Map<*, *> {
        var d: File? = File(System.getProperty("user.dir") ?: ".").absoluteFile
        while (d != null) {
            val f = File(d, "netprobe_app/app/src/main/assets/info.bplist")
            if (f.isFile) return BPlist.parse(f.readBytes()) as Map<*, *>
            val g = File(d, "host/gm_ccpa/netprobe_app/app/src/main/assets/info.bplist")
            if (g.isFile) return BPlist.parse(g.readBytes()) as Map<*, *>
            d = d.parentFile
        }
        throw AssertionError("assets/info.bplist not found above ${System.getProperty("user.dir")}")
    }

    @Test fun txtRecordEqualsServedInfo() {
        val info = infoPlist()
        val txt = CarPlayRx.txtRecordFor(CarPlayRx.BOX_DEVICE_ID, CarPlayRx.BOX_PI)
        assertEquals(info["deviceID"], txt["deviceid"])
        assertEquals(info["pi"], txt["pi"])
        assertEquals(info["model"], txt["model"])
        assertEquals(info["sourceVersion"], txt["srcvers"])
        assertEquals(info["name"], CarPlayRx.ACCESSORY_NAME)
        // `features` is one 64-bit value in /info and "0x<low32>,0x<high32>" in TXT.
        val f = info["features"] as Long
        assertEquals("0x%08X,0x%X".format(f and 0xFFFFFFFFL, f ushr 32), txt["features"])
        // statusFlags 4 <-> flags "0x4"
        assertEquals("0x%X".format(info["statusFlags"] as Long), txt["flags"])
        assertEquals(listOf("deviceid", "features", "flags", "model", "protovers", "pi", "srcvers"), txt.keys.toList())
    }
}
