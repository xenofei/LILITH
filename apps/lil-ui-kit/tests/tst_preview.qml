// Live previews (APP-031, APP-037): the bridge takes each frame out of its preview:<job_id>
// event and hands it to the image provider, which keeps one frame per job, until the job ends;
// QML sees only image://lilpreview/<job_id>/<frame> URLs, a new one for every frame, never image
// bytes. FakeCore.emitFromCore runs the bridge's own event step, as Core's events do.
import QtQuick
import QtTest
import Lilith.Core
import Lilith.UiKit

TestCase {
    id: tc
    name: "Preview"

    // A 2x1 and a 3x1 PNG: their width tells the frames apart.
    readonly property string twoWide: "iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAIAAAB7QOjdAAAAD0lEQVR4nGP4z8Dwn4EBAAj+Af9IxWaQAAAAAElFTkSuQmCC"
    readonly property string threeWide: "iVBORw0KGgoAAAANSUhEUgAAAAMAAAABCAIAAACUgoPjAAAADUlEQVR4nGNgYPgPQQAL/gL+kc2Z/gAAAABJRU5ErkJggg=="

    Component {
        id: fakeComponent
        FakeCore {}
    }

    Component {
        id: previewComponent
        PreviewImage {}
    }

    // A plain image of one URL, as a delegate that kept an old one would show it.
    Component {
        id: imageComponent
        Image {
            cache: false
            asynchronous: true
        }
    }

    Component {
        id: logComponent
        EventLogModel {}
    }

    // Emits a frame of `job` as the core does; the URL its event gives QML.
    function frame(core, job, b64) {
        let url = null
        const grab = (topic, kind, seq, json) => {
            if (topic === "preview:" + job)
                url = JSON.parse(json).url
        }
        core.coreEvent.connect(grab)
        core.emitFromCore("preview:" + job, "event", {format: "png", data_b64: b64})
        core.coreEvent.disconnect(grab)
        return url
    }

    function loaded(source) {
        const img = createTemporaryObject(imageComponent, tc, {source: source})
        tryVerify(() => img.status === Image.Ready || img.status === Image.Error)
        return img
    }

    function test_each_frame_has_a_unique_url_and_only_the_newest_is_kept() {
        const core = createTemporaryObject(fakeComponent, tc)
        const preview = createTemporaryObject(previewComponent, tc, {core: core, jobId: "j_1"})
        verify(core.subscribed.indexOf("preview:j_1") >= 0)
        verify(core.subscribed.indexOf("job:j_1") >= 0)
        verify(!preview.cache, "caching off")
        compare(preview.source.toString(), "")

        frame(core, "j_1", twoWide)
        const first = preview.source.toString()
        verify(first.startsWith("image://lilpreview/j_1/"), first)
        tryCompare(preview, "status", Image.Ready)
        compare(preview.implicitWidth, 2)

        frame(core, "j_1", threeWide)
        const second = preview.source.toString()
        verify(second.startsWith("image://lilpreview/j_1/"), second)
        verify(second !== first, "a new URL for every frame")
        tryCompare(preview, "implicitWidth", 3)

        // One frame per job: the old URL now gives the newest frame.
        const old = loaded(first)
        compare(old.status, Image.Ready)
        compare(old.implicitWidth, 3)

        // Another job's frame is its own.
        const third = frame(core, "j_2", twoWide)
        verify(third.startsWith("image://lilpreview/j_2/"), third)
        compare(loaded(third).implicitWidth, 2)
        compare(loaded(second).implicitWidth, 3)
    }

    function test_a_finished_job_drops_its_frame() {
        const core = createTemporaryObject(fakeComponent, tc)
        const preview = createTemporaryObject(previewComponent, tc, {core: core, jobId: "j_3"})
        frame(core, "j_3", twoWide)
        const url = preview.source.toString()
        tryCompare(preview, "status", Image.Ready)

        // A progress event isn't the end.
        core.emitFromCore("job:j_3", "event", {kind: "progress", progress: {value: 1, max: 2}})
        compare(loaded(url).status, Image.Ready)

        core.emitFromCore("job:j_3", "event", {kind: "status", job: {job_id: "j_3", status: "succeeded"}})
        compare(preview.source.toString(), "", "the preview shows nothing once the job ends")
        compare(loaded(url).status, Image.Error, "the frame is gone")

        // The jobs topic says the same for a job nobody shows.
        const other = frame(core, "j_4", twoWide)
        compare(loaded(other).status, Image.Ready)
        core.emitFromCore("jobs", "event", {kind: "job", job: {job_id: "j_4", status: "failed"}})
        compare(loaded(other).status, Image.Error)

        // A job that ended while the app was away: after the reconnect, job:<id> gets a
        // snapshot of the job itself, its final status at the top (review M-1).
        const away = createTemporaryObject(previewComponent, tc, {core: core, jobId: "j_6"})
        const awayUrl = frame(core, "j_6", twoWide)
        tryCompare(away, "status", Image.Ready)
        core.emitFromCore("job:j_6", "snapshot", {job_id: "j_6", kind: "comfyui.workflow", status: "running"})
        compare(away.source.toString(), awayUrl, "a running job keeps its frame")
        core.emitFromCore("job:j_6", "snapshot", {job_id: "j_6", kind: "comfyui.workflow", status: "cancelled"})
        compare(away.source.toString(), "")
        compare(loaded(awayUrl).status, Image.Error)
    }

    function test_no_image_bytes_reach_qml_properties() {
        const core = createTemporaryObject(fakeComponent, tc)
        const log = createTemporaryObject(logComponent, tc, {core: core})
        const preview = createTemporaryObject(previewComponent, tc, {core: core, jobId: "j_5"})
        let seen = null
        core.coreEvent.connect(tc, (topic, kind, seq, json) => {
            if (topic === "preview:j_5")
                seen = JSON.parse(json)
        })
        frame(core, "j_5", twoWide)

        // The event QML gets: the frame's format, size and URL.
        verify(seen !== null)
        compare(Object.keys(seen).sort(), ["bytes", "format", "url"])
        compare(seen.format, "png")
        compare(seen.bytes, 72)
        compare(seen.url, preview.source.toString())

        // Nothing QML holds carries the bytes: the preview's properties, the log's rows.
        for (const key in preview) {
            const value = preview[key]
            if (typeof value === "string")
                verify(value.indexOf(twoWide) < 0 && value.length < 200, key)
        }
        const row = log.get(log.rowCount() - 1)
        compare(row.topic, "preview:j_5")
        verify(row.text.indexOf(twoWide) < 0, row.text)
        verify(row.text.indexOf("image://lilpreview/j_5/") >= 0, row.text)
    }
}
