# About the project

## Why was Chronoscope built?

Buildings are some of the largest things humans build, and we take them for granted because they're everywhere. Hundreds of people, heavy machinery, structures hundreds of feet tall, expected to last decades or longer, with enormous variation in engineering, purpose, and architecture. They outlast the people who built them, and that persistence makes the built environment one of the most powerful ways to contextualize history. The interest in inhabiting the past through physical places is deeply common, from then-and-now photographs to urban exploration to millions traveling to walk through old cities. What's been missing is a way to explore it all at scale. Recent AI advances make it possible to build the knowledge base from existing open datasets without needing a community to manually annotate millions of images first. For the full story, see the [About page](/about).

## How does Chronoscope work?

Chronoscope is a wiki with one rule: every claim in it points back to a source anyone can go and check, be that a photograph, an archive record, a map, or a Wikipedia article. That rule is what lets AI contribute here at all. A machine-made claim and a human-made claim arrive in the same form, with their evidence attached, and neither is believed on the strength of who submitted it.

What makes our model work is that every claim has to fit alongside all the others into a single coherent picture of the world, and when one doesn't fit, that's worth knowing. Wikipedia might say a church was demolished in 1896, while a photo archive dates a picture of that same church to 1902. Both can't be true: either one of the dates is wrong, or the demolition didn't happen the way it was recorded, or one of the assertions is about a different building. Chronoscope catches the collision and raises it as an open question for contributors to research instead of quietly picking a winner.

The same reasoning machinery fills in what nobody wrote down. Imagine being handed an undated photograph of two buildings you don't recognize. You could still infer something based on that photo: both buildings were standing at the same moment, whatever moment that was. That's all Chronoscope knows at first, and it holds onto the knowledge. Then someone recognizes one of the buildings: a hall built in 1923 and torn down in 1931. The previously undated photograph is now pinned to those eight years, and some knowledge about the second building comes along with it: it was standing somewhere in that window, so it must have been built before 1931. If it was ever demolished, that was after 1923. Nobody asserted a single fact about that second building, but Chronoscope knows some of its history purely by inference. It's the reasoning you'd do yourself with the photo in your hand, run across millions of images and efficiently redone every time new evidence arrives.

This is also what makes automated research safe to build on. A vision model shown a ruined church may announce that it's a particular Dresden church lost in the 1945 bombings: confident, plausible, and impossible to check at face value. So we don't ask it for conclusions. We ask for the smaller things we can validate independently by looking only at the image: roof styles, numbers of floors, which buildings are next to one another, what a sign says, and so on. When those details fit what's already known about a place and a period, the evidence is admitted and the reasoning above does the rest. When they don't, the mismatch surfaces like any other disagreement.

## How does Chronoscope handle dates and locations that aren't precise?

Many historical sources give approximate information: "circa 1920s," "somewhere near the waterfront," "between 1914 and 1918." Chronoscope stores that vagueness as given and reasons over it directly: "sometime in the 1920s" is a real value in its own right, and two vague dates can still contradict each other, or combine into something sharper than either alone. A value narrows when evidence genuinely narrows it.

## What do the markers on the map mean? {#existence-states}

The map always shows a particular year, and every marker answers one question: did the sources say this place stood then?

A **solid marker** means we presume it stood. Either a source documents the place that year, or it was built earlier and nothing records its demolition. Almost nothing is documented continuously, so a palace recorded once in 1343 is presumed to stand every day after until something says otherwise. Treating that as doubt would put the whole map in doubt.

An **orange ring** means the sources disagree. A record says the building came down in 1896; a dated photograph shows it standing in 1902. Both can't be right, so we mark the place as contested and leave the question open. Open the entity to see which claims collide.

A **dashed ring** means we know where the place is but not when it stood. Nothing on record puts it standing or gone in the year you're looking at, so it stays on the map as a possibility. Evidence carries forward and not back: a building sighted in 1900 is presumed to stand ever after, and is simply unknown before it. Give that same building a construction date and scrubbing past it takes the marker away instead. A plain marker goes hollow inside the ring; one carrying a photograph fades behind it.

**Nothing at all** means the sources put its demolition before that year, or its construction after. Scrub back and the demolished ones reappear.

A **badge** is a cluster: several places too close together to draw separately at this zoom. It stands for a group and carries no answer of its own. Zoom in and it splits into individual markers, each with one.

## How does the automated image analysis work?

AI models analyze images in a pipeline of focused steps. First, individual buildings are outlined in each photo so they can be studied separately. Then a visual similarity engine finds the same building across photos from different eras. Finally, a vision-language model describes what it sees: building type, approximate age, architectural style, visible damage. Each step is independently checkable, and every AI-generated conclusion is grounded in the specific photo or source it came from. For more technical details on the specific models used, see our [GitHub documentation](https://github.com/copumpkin/chronoscope).

## Can the AI make mistakes?

Yes, and the system is designed around that assumption. Every AI-generated claim requires a citation to the source material it was derived from. Citations are verified in two ways: first, a simple check that the cited content actually exists at the linked source. Second, a separate AI model evaluates whether the evidence actually supports the claim. Human edits go through the same scrutiny. This makes it hard for errors, misinformation, or deliberate disinformation to spread unchecked. Hard doesn't mean impossible, which is why Chronoscope works like a wiki: if you see something wrong, flag it or fix it. The system is designed to make that easy and even fun.

## How is Chronoscope different from OpenStreetMap, OpenHistoricalMap, Wikidata, Pleiades, and similar projects? {#vs-other-projects}

Most existing projects are organized around a map, a gazetteer, or a structured fact base. Chronoscope is organized around **media and the things visible in it**: photographs, maps, drawings, and the buildings and places they depict over time. We use data and identifiers from projects like OpenStreetMap, Wikidata, and OpenHistoricalMap, and we intend to contribute data back as we build confidence in our results. See [Related work and where we fit](/related-work) for details on each project and how we integrate.

# Safety and trust

## How does Chronoscope handle privacy and safety?

Chronoscope is about buildings and places, not people. But detailed information about abandoned or vulnerable sites could attract vandalism or theft, so we design around these risks from the start.

Sensitive locations (abandoned buildings, structures in conflict zones, active urban exploration sites) support distance-based visibility: precise coordinates are only revealed when you're physically nearby or have earned sufficient trust through contributions. Sites can also become fully public after they're no longer sensitive. Every change is versioned with full history, and tools for detecting and reverting vandalism are built in.

Contributors operate under a trust system. New users' submissions are moderated. As your track record builds, you earn faster publishing and eventually moderation privileges. The goal is to make history accessible without creating a playbook for bad actors.

# Contributing

## Are you interested in collaborations with historians and researchers?

Absolutely. Chronoscope is built to complement existing research, not replace it. If you're a historian, archivist, or researcher with domain expertise, we'd love to hear from you. We're especially interested in ingesting complete collections from historical photo archives and institutional datasets. We're building tools to make it easy to bring in large datasets without manual annotation. If you have a collection you think belongs here, [reach out on GitHub](https://github.com/copumpkin/chronoscope/issues).

## Is it open source?

Yes! Chronoscope is open source under the MIT license. The codebase includes the API server, analysis pipeline, iOS app, and this web frontend, all sharing types and contracts via OpenAPI.

## Who runs Chronoscope, and how is it funded?

Chronoscope is a project of the Chronoscope Foundation, formed to keep the project independent and accessible. {{foundation_status}} The code is MIT-licensed and the knowledge graph is released under Creative Commons Attribution 4.0, so the work stays usable regardless of what becomes of any one organization. The Foundation will be funded by donations and grants.

## How can I contribute?

We're working on the first release of the full site, which will let you contribute directly to the knowledge graph: linking photos to locations, adding historical context, and resolving conflicts in the data. In the meantime, code contributions are welcome. Check out the [GitHub repository](https://github.com/copumpkin/chronoscope) for the codebase, data ingestion pipelines, and discussion in Issues.
