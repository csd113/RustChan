//! Bounded SPS metadata regressions; these never invoke the picture allocator.

use anyhow::{ensure, Result};
use heic_rs::props::hvcc::{NalArray, NAL_PPS, NAL_SPS};

/// Build only the SPS prefix inspected by preflight, with one constant SPS ID.
fn sps(width: u32, height: u32) -> Result<Vec<u8>> {
    let mut bits = vec![false; 104];
    for value in [0, 1, width, height] {
        let code = value
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("SPS value overflow"))?;
        let count = 32_u32.saturating_sub(code.leading_zeros());
        bits.extend(std::iter::repeat_n(
            false,
            usize::try_from(count.saturating_sub(1))?,
        ));
        for shift in (0..count).rev() {
            bits.push(code & (1_u32 << shift) != 0);
        }
    }
    let mut bytes = vec![NAL_SPS << 1_i32, 1];
    for chunk in bits.chunks(8) {
        let mut byte = 0_u8;
        for (offset, bit) in chunk.iter().enumerate() {
            if *bit {
                byte |= 1_u8 << (7_usize.saturating_sub(offset));
            }
        }
        bytes.push(byte);
    }
    Ok(bytes)
}

/// Array/header disagreement and oversized duplicate IDs must fail before decode.
#[test]
fn all_sps_entries_are_checked_even_when_relabelled_or_replaced() -> Result<()> {
    let small = sps(16, 16)?;
    let large = sps(10_000, 10_000)?;
    let array = |nal_type, nals| NalArray {
        completeness: true,
        nal_type,
        nals,
    };
    ensure!(
        super::preflight_arrays(&[array(NAL_SPS, vec![small.as_slice()])])? == 256,
        "small SPS rejected"
    );
    let mismatch = super::preflight_arrays(&[array(NAL_PPS, vec![small.as_slice()])])
        .err()
        .ok_or_else(|| anyhow::anyhow!("mislabelled SPS admitted"))?;
    ensure!(
        mismatch.to_string().contains("disagrees"),
        "actual NAL type was not checked"
    );
    for arrays in [
        vec![
            array(NAL_SPS, vec![small.as_slice()]),
            array(NAL_PPS, vec![large.as_slice()]),
        ],
        vec![array(NAL_SPS, vec![small.as_slice(), large.as_slice()])],
        vec![array(NAL_SPS, vec![large.as_slice(), small.as_slice()])],
    ] {
        ensure!(
            super::preflight_arrays(&arrays).is_err(),
            "oversized replacement passed preflight"
        );
    }
    for nal in [&[0x42][..], &[0xc2, 1][..], &[0x42, 0][..]] {
        ensure!(
            super::preflight_arrays(&[array(NAL_SPS, vec![nal])]).is_err(),
            "invalid NAL header admitted"
        );
    }
    Ok(())
}

/// Primary and alpha grids must visit their coded tiles rather than trusting
/// the small declared grid/ispe geometry. Context construction does no decoding.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "keeps the complete tiny HEIC grid/alpha metadata fixture visible beside its pre-allocation assertions"
)]
fn primary_and_alpha_grid_tiles_use_the_same_preflight() -> Result<()> {
    use heic_rs::{
        boxes::BoxHeader,
        context::Context,
        ftyp::{Brand, FileType},
        meta::{
            iinf::ItemInfo,
            iloc::{Construction, Extent, ItemLocation},
            iprp::{Association, Properties},
            iref::{ItemReference, RefKind},
            Meta,
        },
    };
    let small = sps(16, 16)?;
    let large = sps(10_000, 10_000)?;
    let configuration = |nal: &[u8]| -> Result<Vec<u8>> {
        let mut data = vec![0; 23];
        *data
            .get_mut(0)
            .ok_or_else(|| anyhow::anyhow!("config version"))? = 1;
        *data
            .get_mut(21)
            .ok_or_else(|| anyhow::anyhow!("NAL length size"))? = 3;
        *data
            .get_mut(22)
            .ok_or_else(|| anyhow::anyhow!("array count"))? = 1;
        data.extend_from_slice(&[NAL_SPS, 0, 1]);
        data.extend_from_slice(&u16::try_from(nal.len())?.to_be_bytes());
        data.extend_from_slice(nal);
        Ok(data)
    };
    let small_config = configuration(&small)?;
    let large_config = configuration(&large)?;
    let canvas = [0, 0, 0, 0, 0, 0, 0, 16, 0, 0, 0, 16];
    let auxiliary = b"\0\0\0\0urn:mpeg:hevc:2015:auxid:1\0";
    let grid = [0, 0, 0, 0, 0, 16, 0, 16];
    let property = |boxtype, payload| BoxHeader {
        boxtype,
        payload,
        uuid: None,
        offset: 0,
    };
    let association = |index| Association {
        index,
        essential: false,
    };
    let item = |id, item_type| ItemInfo {
        id,
        item_type,
        name: "",
        content_type: None,
        item_uri_type: None,
        hidden: false,
    };
    for bad_tile in [2_u32, 4] {
        let primary_config = if bad_tile == 2 {
            &large_config
        } else {
            &small_config
        };
        let alpha_config = if bad_tile == 4 {
            &large_config
        } else {
            &small_config
        };
        let context = Context {
            file: &[],
            ftyp: FileType {
                major: Brand::Heic,
                effective: Brand::Heic,
                minor_version: 0,
                compatible_count: 0,
            },
            meta: Meta {
                handler: *b"pict",
                primary: 1,
                items: vec![
                    item(1, *b"grid"),
                    item(2, *b"hvc1"),
                    item(3, *b"grid"),
                    item(4, *b"hvc1"),
                ],
                refs: vec![
                    ItemReference {
                        kind: RefKind::Dimg,
                        from: 1,
                        to: vec![2],
                    },
                    ItemReference {
                        kind: RefKind::Dimg,
                        from: 3,
                        to: vec![4],
                    },
                    ItemReference {
                        kind: RefKind::Auxl,
                        from: 3,
                        to: vec![1],
                    },
                ],
                locations: [1, 3]
                    .into_iter()
                    .map(|id| ItemLocation {
                        id,
                        construction: Construction::Idat,
                        base_offset: 0,
                        extents: vec![Extent {
                            index: 0,
                            offset: 0,
                            length: 8,
                        }],
                    })
                    .collect(),
                props: Properties {
                    boxes: vec![
                        property(*b"hvcC", primary_config),
                        property(*b"hvcC", alpha_config),
                        property(*b"ispe", &canvas),
                        property(*b"auxC", auxiliary),
                    ],
                    assoc: vec![
                        (1, vec![association(3)]),
                        (2, vec![association(1), association(3)]),
                        (3, vec![association(3), association(4)]),
                        (4, vec![association(2), association(3)]),
                    ],
                },
                idat: Some(&grid),
            },
        };
        ensure!(
            context.alpha_item(1)? == Some(3),
            "alpha fixture not recognized"
        );
        ensure!(
            super::preflight_item(&context, if bad_tile == 2 { 1 } else { 3 }).is_err(),
            "oversized grid tile escaped preflight"
        );
        ensure!(
            super::preflight_item(&context, if bad_tile == 2 { 3 } else { 1 })? == 256,
            "safe tile path rejected"
        );
    }
    Ok(())
}
